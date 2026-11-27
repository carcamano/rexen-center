//! Updates in the window (this console's own; the original had none): the
//! automatic check, Check for Updates…, the update button at the top of
//! the window and the Software Update dialog. The flow itself is
//! [`xen_update::updater`]; this module draws it and passes the clicks on.
//!
//! "Restart Now" and "Quit and Install" leave the update in
//! [`UpdateUi::post_exit`] and close the window like File > Quit, so that
//! the sessions are logged out first; `main` completes the update once the
//! window is gone.

use std::sync::{Arc, Mutex, PoisonError};

use egui::{
    Align, Align2, Button, Context, Layout, Margin, ProgressBar, RichText, ScrollArea, TextEdit,
    Ui, ViewportCommand,
};
use xen_update::updater::{Offer, Phase, PostExit, ReadyKind, Untrusted, Updater};
use xen_update::{Arch, InstallMethod, UnknownReason, apply};

use crate::branding;
use crate::i18n::fill;
use crate::update_strings::update_strings;
use crate::window_frame::dialog_window;

/// The dialog's width (points).
const WIDTH: f32 = 520.0;

/// The update shared with `main`, completed once the window has closed.
pub type PostExitSlot = Arc<Mutex<Option<PostExit>>>;

/// The update state of the window.
#[derive(Default)]
pub struct UpdateUi {
    /// `None`: updates are not available (the tests' windows).
    updater: Option<Updater>,
    pub dialog_open: bool,
    /// The start was reported to the version that installed this one.
    started: bool,
    post_exit: PostExitSlot,
    /// Messages for the window's errors list.
    pub errors: Vec<String>,
}

impl UpdateUi {
    pub fn new(updater: Updater, post_exit: PostExitSlot) -> Self {
        Self {
            updater: Some(updater),
            post_exit,
            ..Self::default()
        }
    }

    /// Check for Updates… can be chosen.
    pub fn enabled(&self) -> bool {
        self.updater.is_some()
    }

    /// Check for Updates…: checks now and shows the result.
    pub fn check_now(&mut self) {
        if let Some(updater) = &self.updater {
            let busy = matches!(
                updater.view().phase,
                Phase::Downloading { .. } | Phase::Preparing(_) | Phase::Ready { .. }
            );
            if !busy {
                updater.check(true);
            }
            updater.undismiss();
            self.dialog_open = true;
        }
    }

    /// Every frame, before the window's contents: reports the start, runs
    /// the automatic check when due.
    pub fn frame(&mut self, ctx: &Context, automatic: bool) {
        let Some(updater) = &self.updater else {
            return;
        };
        if !self.started {
            self.started = true;
            let by_update = apply::acknowledge_start();
            apply::cleanup_after_start(updater.method(), by_update);
            apply::remove_stale_downloads(xen_update::updater::DOWNLOAD_PREFIX);
        }
        let view = updater.view();
        if let Some(rolled_back) = view.rolled_back {
            self.errors.push(fill(
                update_strings().rolled_back,
                &[
                    ("version", &rolled_back.version),
                    ("reason", &rolled_back.reason),
                ],
            ));
            updater.clear_rolled_back();
        }
        if let Some(wait) = updater.tick(automatic) {
            ctx.request_repaint_after(wait);
        }
    }

    /// The update button, right-aligned at the top of the window, while a
    /// newer version is offered.
    pub fn toolbar_button(&mut self, ui: &mut Ui) {
        let Some(version) = self.updater.as_ref().and_then(|u| u.view().button) else {
            return;
        };
        let u = update_strings();
        let version = version.to_string();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let visuals = ui.visuals();
            let text = RichText::new(fill(u.button, &[("version", &version)]))
                .color(visuals.selection.stroke.color)
                .strong();
            let button = Button::new(text).fill(visuals.selection.bg_fill);
            let hint = fill(
                u.button_hint,
                &[("product", branding::PRODUCT_NAME), ("version", &version)],
            );
            if ui.add(button).on_hover_text(hint).clicked() {
                self.dialog_open = true;
            }
        });
    }

    /// The Software Update dialog.
    pub fn window(&mut self, ctx: &Context) {
        if !self.dialog_open {
            return;
        }
        let Some(updater) = self.updater.clone() else {
            self.dialog_open = false;
            return;
        };
        let view = updater.view();
        if matches!(view.phase, Phase::Idle) {
            self.dialog_open = false;
            return;
        }
        let u = update_strings();
        let mut open = true;
        let mut close = false;
        dialog_window(u.dialog_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .inner_margin(Margin::same(14))
            .min_width(WIDTH)
            .max_width(WIDTH)
            .show(ctx, |ui| {
                close = self.contents(ui, &updater, &view.phase);
            });
        if close || !open {
            self.dialog_open = false;
            // A result seen needs no keeping.
            if matches!(
                view.phase,
                Phase::UpToDate | Phase::Failed { offer: None, .. }
            ) {
                updater.later();
            }
        }
    }

    /// The dialog's contents for `phase`; returns whether to close it.
    fn contents(&mut self, ui: &mut Ui, updater: &Updater, phase: &Phase) -> bool {
        let u = update_strings();
        let product = branding::PRODUCT_NAME;
        let running = updater.running().to_string();
        match phase {
            Phase::Idle => true,
            Phase::Checking { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(u.checking);
                });
                buttons(ui, |ui| ui.button(u.close).clicked())
            }
            Phase::UpToDate => {
                ui.label(fill(
                    u.up_to_date,
                    &[("product", product), ("version", &running)],
                ));
                buttons(ui, |ui| ui.button(u.close).clicked())
            }
            Phase::Failed {
                message,
                offer: None,
            } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    fill(u.check_failed, &[("error", message)]),
                );
                buttons(ui, |ui| ui.button(u.close).clicked())
            }
            Phase::Available(offer)
            | Phase::Failed {
                offer: Some(offer), ..
            } => {
                self.offer(ui, updater, offer);
                if let Phase::Failed { message, .. } = phase {
                    ui.add_space(6.0);
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        fill(u.failed, &[("error", message)]),
                    );
                }
                ui.add_space(8.0);
                buttons(ui, |ui| {
                    let mut close = false;
                    if offer.installable() {
                        let label = match updater.method() {
                            InstallMethod::WindowsMsi { .. } => u.install_msi,
                            InstallMethod::LinuxDeb | InstallMethod::LinuxRpm => u.install_package,
                            _ => u.install_restart,
                        };
                        if ui.button(RichText::new(label).strong()).clicked() {
                            updater.install();
                        }
                    } else if ui.button(RichText::new(u.release_page).strong()).clicked() {
                        open_page(offer.page());
                    }
                    if ui.button(u.later).clicked() {
                        updater.later();
                        close = true;
                    }
                    if ui.button(u.skip).clicked() {
                        updater.skip();
                        close = true;
                    }
                    close
                })
            }
            Phase::Downloading { offer, done, total } => {
                let file = offer.package.as_ref().map_or("", |p| p.name.as_str());
                ui.label(fill(u.downloading, &[("file", file)]));
                ui.add_space(4.0);
                let done_text = size(*done);
                let (fraction, text) = match total {
                    Some(total) if *total > 0 => (
                        *done as f32 / *total as f32,
                        fill(
                            u.progress,
                            &[("done", &done_text), ("total", &size(*total))],
                        ),
                    ),
                    _ => (0.0, done_text),
                };
                ui.add(ProgressBar::new(fraction).text(text).animate(true));
                buttons(ui, |ui| {
                    if ui.button(u.cancel).clicked() {
                        updater.cancel();
                    }
                    false
                })
            }
            Phase::Preparing(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(u.preparing);
                });
                buttons(ui, |ui| {
                    if ui.button(u.cancel).clicked() {
                        updater.cancel();
                    }
                    false
                })
            }
            Phase::Ready { offer, kind } => {
                ui.label(
                    RichText::new(fill(
                        u.available,
                        &[
                            ("product", product),
                            ("version", &offer.version.to_string()),
                            ("running", &running),
                        ],
                    ))
                    .strong(),
                );
                ui.add_space(6.0);
                self.ready(ui, updater, kind)
            }
        }
    }

    /// The offer: versions, installation, notes and the page.
    fn offer(&self, ui: &mut Ui, updater: &Updater, offer: &Offer) {
        let u = update_strings();
        ui.label(
            RichText::new(fill(
                u.available,
                &[
                    ("product", branding::PRODUCT_NAME),
                    ("version", &offer.version.to_string()),
                    ("running", &updater.running().to_string()),
                ],
            ))
            .strong(),
        );
        ui.label(
            RichText::new(fill(
                u.installed_as,
                &[("method", &method_label(updater.method(), updater.arch()))],
            ))
            .weak(),
        );
        ui.add_space(8.0);
        ui.label(RichText::new(u.notes_heading).strong());
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let notes = offer.notes().trim();
                    if notes.is_empty() {
                        ui.label(RichText::new(u.notes_empty).weak());
                    } else {
                        ui.label(notes);
                    }
                });
        });
        if ui.link(u.release_page).clicked() {
            open_page(offer.page());
        }
        ui.add_space(6.0);
        if offer.package.is_none() {
            ui.label(u.no_package);
        } else if let Some(why) = offer.untrusted() {
            ui.label(untrusted_text(why));
        } else {
            ui.label(RichText::new(u.integrity_note).small().weak());
        }
    }

    /// A downloaded and verified update; returns whether to close.
    fn ready(&mut self, ui: &mut Ui, updater: &Updater, kind: &ReadyKind) -> bool {
        let u = update_strings();
        let product = branding::PRODUCT_NAME;
        match kind {
            ReadyKind::Restart | ReadyKind::Installer => {
                let (text, action) = if *kind == ReadyKind::Restart {
                    (u.ready_restart, u.restart_now)
                } else {
                    (u.ready_installer, u.quit_install)
                };
                ui.label(fill(text, &[("product", product)]));
                buttons(ui, |ui| {
                    if ui.button(RichText::new(action).strong()).clicked() {
                        self.complete_on_exit(ui.ctx(), updater);
                    }
                    if ui.button(u.cancel).clicked() {
                        updater.cancel();
                        return true;
                    }
                    false
                })
            }
            ReadyKind::SystemPackage { package, command } => {
                ui.label(u.ready_package);
                ui.add_space(4.0);
                let mut text = command.clone();
                ui.add(
                    TextEdit::singleline(&mut text)
                        .code_editor()
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                ui.label(RichText::new(fill(u.package_restart, &[("product", product)])).weak());
                buttons(ui, |ui| {
                    if ui.button(u.copy_command).clicked() {
                        ui.ctx().copy_text(command.clone());
                    }
                    if ui.button(u.open_installer).clicked()
                        && let Err(e) = apply::open_external(package.as_os_str())
                    {
                        self.errors
                            .push(fill(u.failed, &[("error", &e.to_string())]));
                    }
                    ui.button(u.close).clicked()
                })
            }
        }
    }

    /// Leaves the prepared update for `main` and closes the window as
    /// File > Quit does (the sessions are logged out on exit).
    fn complete_on_exit(&mut self, ctx: &Context, updater: &Updater) {
        if let Some(action) = updater.take_post_exit() {
            *self
                .post_exit
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(action);
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }
}

/// The row of buttons at the dialog's bottom, the main one first, as in
/// the window's other dialogs; returns what `add` returns.
fn buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> bool) -> bool {
    ui.add_space(6.0);
    ui.horizontal(add).inner
}

fn size(bytes: u64) -> String {
    xen_core::format::disk_size(i64::try_from(bytes).unwrap_or(i64::MAX))
}

fn open_page(url: &str) {
    if let Err(e) = apply::open_external(std::ffi::OsStr::new(url)) {
        tracing::warn!(%url, error = %e, "cannot open the release page");
    }
}

/// Why the release is not installed from the window (only its page is
/// offered).
pub fn untrusted_text(why: &Untrusted) -> String {
    let u = update_strings();
    match why {
        Untrusted::NoChecksumFile => u.no_checksums.to_owned(),
        Untrusted::NoSignature => u.no_signature.to_owned(),
        Untrusted::BadSignature(e) => fill(u.bad_signature, &[("error", &e.to_string())]),
    }
}

/// How the installation is described in the dialog.
pub fn method_label(method: &InstallMethod, arch: Arch) -> String {
    let u = update_strings();
    match method {
        InstallMethod::MacBundle { .. } => {
            let arch = if arch == Arch::Aarch64 {
                u.arch_arm
            } else {
                u.arch_intel
            };
            fill(u.method_mac, &[("arch", arch)])
        }
        InstallMethod::WindowsMsi { .. } => u.method_msi.to_owned(),
        InstallMethod::WindowsPortable { .. } => u.method_portable.to_owned(),
        InstallMethod::LinuxDeb => u.method_deb.to_owned(),
        InstallMethod::LinuxRpm => u.method_rpm.to_owned(),
        InstallMethod::LinuxTarball { .. } => u.method_tarball.to_owned(),
        InstallMethod::Unknown(reason) => {
            let reason = match reason {
                UnknownReason::NoExecutable => u.reason_no_exe,
                UnknownReason::DevelopmentBuild => u.reason_development,
                UnknownReason::Translocated => u.reason_translocated,
                UnknownReason::AppImage => u.reason_appimage,
                UnknownReason::UnsupportedSystem => u.reason_unsupported,
                UnknownReason::Unrecognized => u.reason_unrecognized,
            };
            fill(u.method_unknown, &[("reason", reason)])
        }
    }
}

/// Options > Updates.
pub fn options(ui: &mut Ui, check: &mut bool) {
    let u = update_strings();
    ui.label(RichText::new(u.options_heading).strong());
    ui.checkbox(check, u.options_check)
        .on_hover_text(u.options_check_hint);
}
