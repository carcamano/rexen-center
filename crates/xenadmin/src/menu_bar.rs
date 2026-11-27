//! The menus in macOS's menu bar (this console's own; the original was a
//! Windows Forms window with its menus in it).
//!
//! Apple's Human Interface Guidelines put an app's menus in the menu bar at
//! the top of the screen, not in its windows. egui cannot drive AppKit's
//! menus, so on macOS the window describes the whole menu bar every frame
//! ([`Menu`], [`Item`]) and a [`NativeMenuBar`] shows it: `muda`'s on macOS
//! (`crate::macos::MudaMenuBar`), a fake in the tests. [`Shown`] keeps what
//! is on screen, so that only a menu whose items changed is built again, and
//! turns a chosen item back into its command, which the window carries out
//! as it would a click on its own menu.

// The half used by the native menus ([`Shown`], [`ItemId`]) runs on macOS
// only; the tests use all of it everywhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use egui::KeyboardShortcut;

/// Shows the menu bar described by the window.
pub trait NativeMenuBar<C> {
    /// Shows `menus`, left to right. Called every frame: cheap when nothing
    /// changed.
    fn update(&mut self, menus: Vec<Menu<C>>);
    /// The commands of the items chosen since the last call.
    fn take_chosen(&mut self) -> Vec<C>;
}

/// A menu of the menu bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu<C> {
    pub title: String,
    pub role: Role,
    pub items: Vec<Item<C>>,
}

/// What macOS does with a menu besides showing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The application menu: the first, titled with the app's name.
    App,
    /// The Window menu, where macOS lists the app's windows.
    Window,
    /// The Help menu, where macOS adds its search field.
    Help,
    Other,
}

/// An item of a menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Item<C> {
    /// An item that gives `command` when chosen; with `checked`, a check
    /// item.
    Command {
        label: String,
        enabled: bool,
        checked: Option<bool>,
        shortcut: Option<KeyboardShortcut>,
        command: C,
    },
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<Item<C>>,
    },
    /// A disabled line (an empty menu says so).
    Note(String),
    Separator,
    /// An item that AppKit carries out itself, with its usual shortcut.
    Standard {
        item: Standard,
        label: String,
    },
}

/// The items AppKit carries out itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standard {
    Services,
    /// ⌘H
    Hide,
    /// ⌥⌘H
    HideOthers,
    ShowAll,
    /// ⌃⌘F, on and off.
    FullScreen,
    /// ⌘M
    Minimize,
    Zoom,
    BringAllToFront,
}

impl<C> Item<C> {
    pub fn command(label: impl Into<String>, enabled: bool, command: C) -> Self {
        Self::Command {
            label: label.into(),
            enabled,
            checked: None,
            shortcut: None,
            command,
        }
    }

    pub fn check(label: impl Into<String>, checked: bool, command: C) -> Self {
        Self::Command {
            label: label.into(),
            enabled: true,
            checked: Some(checked),
            shortcut: None,
            command,
        }
    }

    /// The item with `shortcut` (commands only).
    pub fn shortcut(mut self, key: KeyboardShortcut) -> Self {
        if let Self::Command { shortcut, .. } = &mut self {
            *shortcut = Some(key);
        }
        self
    }

    pub fn standard(item: Standard, label: impl Into<String>) -> Self {
        Self::Standard {
            item,
            label: label.into(),
        }
    }

    /// The item without its commands (for comparing what is shown); its
    /// commands go to `commands`, depth first.
    fn split(self, commands: &mut Vec<C>) -> Item<()> {
        match self {
            Self::Command {
                label,
                enabled,
                checked,
                shortcut,
                command,
            } => {
                commands.push(command);
                Item::Command {
                    label,
                    enabled,
                    checked,
                    shortcut,
                    command: (),
                }
            }
            Self::Submenu {
                label,
                enabled,
                items,
            } => Item::Submenu {
                label,
                enabled,
                items: items.into_iter().map(|i| i.split(commands)).collect(),
            },
            Self::Note(label) => Item::Note(label),
            Self::Separator => Item::Separator,
            Self::Standard { item, label } => Item::Standard { item, label },
        }
    }
}

impl<C> Menu<C> {
    /// The menu without its commands, and the commands: those of its
    /// command items, depth first (an item's index among them names it, see
    /// [`ItemId`]).
    pub fn split(self) -> (Menu<()>, Vec<C>) {
        let mut commands = Vec::new();
        let items = self
            .items
            .into_iter()
            .map(|i| i.split(&mut commands))
            .collect();
        let shape = Menu {
            title: self.title,
            role: self.role,
            items,
        };
        (shape, commands)
    }
}

/// Where a native item comes from: its menu, the build of that menu's items
/// and its index among them. Written into the native item's identifier, so
/// that a click on an item since built again is ignored instead of taken
/// for another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemId {
    pub menu: usize,
    pub build: u64,
    pub index: usize,
}

const ID_PREFIX: &str = "rexen";

impl ItemId {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        if parts.next()? != ID_PREFIX {
            return None;
        }
        let id = Self {
            menu: parts.next()?.parse().ok()?,
            build: parts.next()?.parse().ok()?,
            index: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(id)
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{ID_PREFIX}:{}:{}:{}", self.menu, self.build, self.index)
    }
}

/// What the native menus must build after [`Shown::update`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebuild {
    /// The menus are as they were (commands may have changed).
    Nothing,
    /// These menus' titles and items changed.
    Menus(Vec<usize>),
    /// The menus themselves changed (the first update, or another number or
    /// order of menus): every menu, in the bar.
    Bar,
}

/// The menus on screen: their shapes, builds and commands.
pub struct Shown<C> {
    menus: Vec<ShownMenu<C>>,
    builds: u64,
}

struct ShownMenu<C> {
    shape: Menu<()>,
    build: u64,
    commands: Vec<C>,
}

impl<C> Default for Shown<C> {
    fn default() -> Self {
        Self {
            menus: Vec::new(),
            builds: 0,
        }
    }
}

impl<C> Shown<C> {
    /// Takes `menus` as shown and says what must be built for that. A menu
    /// that looks the same keeps its items (and their identifiers); only
    /// its commands are replaced.
    pub fn update(&mut self, menus: Vec<Menu<C>>) -> Rebuild {
        let same_bar = menus.len() == self.menus.len()
            && menus
                .iter()
                .zip(&self.menus)
                .all(|(new, old)| new.role == old.shape.role);
        let mut changed = Vec::new();
        let mut shown = Vec::with_capacity(menus.len());
        for (i, menu) in menus.into_iter().enumerate() {
            let (shape, commands) = menu.split();
            let old = same_bar.then(|| &self.menus[i]);
            let build = match old {
                Some(old) if old.shape == shape => old.build,
                _ => {
                    changed.push(i);
                    self.builds += 1;
                    self.builds
                }
            };
            shown.push(ShownMenu {
                shape,
                build,
                commands,
            });
        }
        self.menus = shown;
        if !same_bar {
            Rebuild::Bar
        } else if changed.is_empty() {
            Rebuild::Nothing
        } else {
            Rebuild::Menus(changed)
        }
    }

    /// Menu `index` as shown, and its build.
    pub fn menu(&self, index: usize) -> Option<(&Menu<()>, u64)> {
        self.menus.get(index).map(|m| (&m.shape, m.build))
    }

    pub fn len(&self) -> usize {
        self.menus.len()
    }

    /// The command of the item `id` names, unless its menu was built again
    /// since.
    pub fn command(&self, id: ItemId) -> Option<&C> {
        let menu = self.menus.get(id.menu)?;
        (menu.build == id.build)
            .then(|| menu.commands.get(id.index))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    fn menu(title: &str, items: Vec<Item<u32>>) -> Menu<u32> {
        Menu {
            title: title.to_owned(),
            role: Role::Other,
            items,
        }
    }

    fn vm_menu(enabled: bool, base: u32) -> Menu<u32> {
        menu(
            "VM",
            vec![
                Item::command("Start", enabled, base),
                Item::Separator,
                Item::Submenu {
                    label: "Start on Server".to_owned(),
                    enabled: true,
                    items: vec![
                        Item::command("host-1", true, base + 1),
                        Item::Note("host-2 (checking…)".to_owned()),
                    ],
                },
                Item::command("Shut Down", true, base + 2)
                    .shortcut(KeyboardShortcut::new(Modifiers::COMMAND, Key::D)),
            ],
        )
    }

    #[test]
    fn commands_are_taken_out_depth_first() {
        let (shape, commands) = vm_menu(true, 10).split();
        // Start, host-1 in the submenu, then Shut Down.
        assert_eq!(commands, [10, 11, 12]);
        assert_eq!(shape, vm_menu(true, 0).split().0);
        let Item::Command { shortcut, .. } = &shape.items[3] else {
            panic!("{:?}", shape.items[3]);
        };
        assert_eq!(
            *shortcut,
            Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::D))
        );
    }

    #[test]
    fn identifiers_round_trip() {
        let id = ItemId {
            menu: 3,
            build: 42,
            index: 7,
        };
        assert_eq!(id.to_string(), "rexen:3:42:7");
        assert_eq!(ItemId::parse(&id.to_string()), Some(id));
        for other in [
            "",
            "rexen:1:2",
            "rexen:1:2:3:4",
            "muda:1:2:3",
            "rexen:a:2:3",
        ] {
            assert_eq!(ItemId::parse(other), None, "{other}");
        }
    }

    #[test]
    fn only_changed_menus_are_built_again() {
        let mut shown = Shown::default();
        let file = || menu("File", vec![Item::command("Connect…", true, 1)]);

        // The first update builds the bar.
        assert_eq!(shown.update(vec![file(), vm_menu(false, 10)]), Rebuild::Bar);
        let start = |shown: &Shown<u32>| ItemId {
            menu: 1,
            build: shown.menu(1).unwrap().1,
            index: 0,
        };
        let first = start(&shown);
        assert_eq!(shown.command(first), Some(&10));

        // Another VM in the same state: same items, its commands.
        assert_eq!(
            shown.update(vec![file(), vm_menu(false, 20)]),
            Rebuild::Nothing
        );
        assert_eq!(start(&shown), first);
        assert_eq!(shown.command(first), Some(&20));

        // Start enabled: the VM menu only is built again, and a click on
        // its former items is ignored.
        assert_eq!(
            shown.update(vec![file(), vm_menu(true, 20)]),
            Rebuild::Menus(vec![1])
        );
        assert_eq!(shown.command(first), None);
        assert_eq!(shown.command(start(&shown)), Some(&20));
        assert_eq!(
            shown.command(ItemId {
                menu: 0,
                build: shown.menu(0).unwrap().1,
                index: 0,
            }),
            Some(&1)
        );

        // A renamed menu is built again; another set of menus, the bar.
        let mut renamed = file();
        renamed.title = "[[File]]".to_owned();
        assert_eq!(
            shown.update(vec![renamed, vm_menu(true, 20)]),
            Rebuild::Menus(vec![0])
        );
        assert_eq!(shown.update(vec![file()]), Rebuild::Bar);
        assert_eq!(shown.len(), 1);
        let mut help = file();
        help.role = Role::Help;
        assert_eq!(shown.update(vec![help]), Rebuild::Bar);
    }
}
