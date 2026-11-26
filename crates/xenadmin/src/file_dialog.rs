//! Native open, save and folder dialogs behind the path fields.
//!
//! Every place that takes a file or folder path keeps its text field (a
//! typed or pasted path works as before) and gains a "Browse..." button that
//! opens the platform's dialog (`rfd`: Cocoa on macOS, the common dialogs on
//! Windows, the XDG desktop portal on Linux) and writes the chosen path into
//! the field, as the original's `OpenFileDialog`/`SaveFileDialog` buttons
//! did. The dialog is modal: the window waits while it is open.
//!
//! Tests never open a native dialog: they queue the answer the "user" gives
//! ([`answer_next`]) and the button takes it instead.

use std::path::{Path, PathBuf};

use egui::Ui;

use crate::i18n::strings;

/// One file type of a dialog's filter list: a name (from the catalogue) and
/// its extensions without the dot.
#[derive(Debug, Clone, Copy)]
pub struct Filter {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
}

/// What the dialog picks.
#[derive(Debug, Clone)]
pub enum Pick {
    /// An existing file.
    Open(Vec<Filter>),
    /// A file to write; the field's file name is suggested.
    Save(Vec<Filter>),
    /// A folder.
    Folder,
}

/// A "Browse..." button for the path in `field`; `true` when a path was
/// chosen (and written into `field`).
pub fn browse_button(ui: &mut Ui, field: &mut String, pick: Pick) -> bool {
    browse_button_labelled(ui, strings().browse, field, pick)
}

/// [`browse_button`] with its own label (several on one page).
pub fn browse_button_labelled(ui: &mut Ui, label: &str, field: &mut String, pick: Pick) -> bool {
    if !ui.button(label).clicked() {
        return false;
    }
    match choose(field, pick) {
        Some(path) => {
            *field = path.display().to_string();
            true
        }
        None => false,
    }
}

/// Asks for a path; `None` when the dialog is cancelled.
fn choose(current: &str, pick: Pick) -> Option<PathBuf> {
    #[cfg(test)]
    {
        let _ = current;
        tests::take_answer(pick)
    }
    #[cfg(not(test))]
    {
        native(current, pick)
    }
}

#[cfg(not(test))]
fn native(current: &str, pick: Pick) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new();
    let (dir, name) = split(current);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    let filters: &[Filter] = match &pick {
        Pick::Open(f) | Pick::Save(f) => f,
        Pick::Folder => &[],
    };
    for f in filters {
        dialog = dialog.add_filter(f.name, f.extensions);
    }
    if !filters.is_empty() {
        dialog = dialog.add_filter(strings().filter_all, &["*"]);
    }
    let chosen = match pick {
        Pick::Open(_) => dialog.pick_file(),
        Pick::Save(_) => {
            if let Some(name) = name {
                dialog = dialog.set_file_name(name);
            }
            dialog.save_file()
        }
        Pick::Folder => dialog.pick_folder(),
    };
    tracing::debug!(chosen = chosen.is_some(), "file dialog");
    chosen
}

/// The folder to start in and the file name to suggest, from the field.
fn split(current: &str) -> (Option<PathBuf>, Option<String>) {
    let current = current.trim();
    if current.is_empty() {
        return (None, None);
    }
    let path = Path::new(current);
    if path.is_dir() {
        return (Some(path.to_path_buf()), None);
    }
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
        .map(Path::to_path_buf);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    (dir, name)
}

// ---- the filters of each place ----------------------------------------------
// Built at each call, so their names follow the language.

/// XVA files (`XVA Files (*.xva)`), for exporting one VM.
pub fn xva_filters() -> Vec<Filter> {
    let s = strings();
    vec![Filter {
        name: s.filter_xva,
        extensions: &["xva"],
    }]
}

/// Everything the Import wizard reads (`IMPORT_SOURCE_FILETYPES`).
pub fn import_filters() -> Vec<Filter> {
    let s = strings();
    vec![
        Filter {
            name: s.filter_import_all,
            extensions: &[
                "ovf", "ova", "gz", "vhd", "vmdk", "img", "raw", "iso", "xva",
            ],
        },
        Filter {
            name: s.filter_ovf,
            extensions: &["ovf", "ova", "gz"],
        },
        Filter {
            name: s.filter_disk_images,
            extensions: &["vhd", "vmdk", "img", "raw", "iso"],
        },
        Filter {
            name: s.filter_xva,
            extensions: &["xva", "gz"],
        },
    ]
}

/// TLS certificates (`CERTIFICATE_FILETYPES`).
pub fn certificate_filters() -> Vec<Filter> {
    let s = strings();
    vec![Filter {
        name: s.filter_certificates,
        extensions: &["pem", "crt", "cer"],
    }]
}

/// TLS private keys (`CERTIFICATE_KEY_FILETYPES`).
pub fn key_filters() -> Vec<Filter> {
    let s = strings();
    vec![Filter {
        name: s.filter_keys,
        extensions: &["pem", "key"],
    }]
}

/// Server backups (`.xbk`).
pub fn backup_filters() -> Vec<Filter> {
    let s = strings();
    vec![Filter {
        name: s.filter_backups,
        extensions: &[crate::pool_ui::BACKUP_EXTENSION],
    }]
}

/// Saved searches (`XENSEARCH_SAVED_SEARCH`).
pub fn search_filters() -> Vec<Filter> {
    let s = strings();
    vec![Filter {
        name: s.filter_searches,
        extensions: &["xensearch"],
    }]
}

#[cfg(test)]
pub mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use super::*;

    thread_local! {
        static ANSWERS: RefCell<VecDeque<Option<PathBuf>>> =
            const { RefCell::new(VecDeque::new()) };
    }

    /// The next dialog on this thread "chooses" `path` (`None`: cancels).
    pub fn answer_next(path: Option<&str>) {
        ANSWERS.with(|a| a.borrow_mut().push_back(path.map(PathBuf::from)));
    }

    /// The kinds of dialog opened on this thread, oldest first.
    pub fn opened() -> Vec<String> {
        OPENED.with(|o| o.borrow().clone())
    }

    thread_local! {
        static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn take_answer(pick: Pick) -> Option<PathBuf> {
        let kind = match &pick {
            Pick::Open(f) => format!("open {}", f.first().map_or("", |f| f.name)),
            Pick::Save(f) => format!("save {}", f.first().map_or("", |f| f.name)),
            Pick::Folder => "folder".to_owned(),
        };
        OPENED.with(|o| o.borrow_mut().push(kind));
        ANSWERS
            .with(|a| a.borrow_mut().pop_front())
            .expect("a dialog opened without a queued answer")
    }

    #[test]
    fn the_field_suggests_the_folder_and_name() {
        let dir = std::env::temp_dir();
        let path = dir.join("backup.xbk");
        let (d, n) = split(&path.display().to_string());
        assert_eq!(d.as_deref(), Some(dir.as_path()));
        assert_eq!(n.as_deref(), Some("backup.xbk"));
        assert_eq!(split("  "), (None, None));
        assert_eq!(split(&dir.display().to_string()), (Some(dir.clone()), None));
        // A bare name: no folder to start in.
        assert_eq!(
            split("search.xensearch"),
            (None, Some("search.xensearch".into()))
        );
    }

    #[test]
    fn filters_name_their_types() {
        assert!(import_filters()[0].extensions.contains(&"xva"));
        assert_eq!(backup_filters()[0].extensions, ["xbk"]);
        assert_eq!(search_filters()[0].extensions, ["xensearch"]);
        assert_eq!(certificate_filters()[0].name, strings().filter_certificates);
    }
}
