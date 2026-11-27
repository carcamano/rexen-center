//! User-interface strings of the updates (this console's own; the
//! original had none): Check for Updates…, the update button at the top of
//! the window, the Software Update dialog and the Options setting.
//!
//! Like [`crate::i18n::Strings`], one table per language; only English
//! exists. Templates use `{name}` placeholders filled by
//! [`crate::i18n::fill`].

use crate::locale::string_table;

string_table! {
    namespace = "update", struct UpdateStrings, fn update_strings;

    /// Help > Check for Updates… (on macOS, in the application menu).
    menu_check = "Check for Updates…",
    /// The button at the top of the window. `{version}`: the new version.
    button = "Update to {version}",
    /// `{product}`, `{version}`
    button_hint = "{product} {version} is available. Click for the release notes and to install it.",
    dialog_title = "Software Update",
    checking = "Checking for updates…",
    /// `{product}`, `{version}`: the running version.
    up_to_date = "{product} {version} is the latest version.",
    /// `{error}`
    check_failed = "Could not check for updates: {error}",
    /// `{product}`, `{version}`: the new version; `{running}`: this one.
    available = "{product} {version} is available. You have {running}.",
    notes_heading = "Release notes",
    notes_empty = "This release has no notes.",
    release_page = "Open the Release Page",
    /// `{method}`: one of the `method_*` texts.
    installed_as = "Installed as: {method}",
    /// `{arch}`: Apple silicon or Intel.
    method_mac = "macOS app ({arch})",
    arch_arm = "Apple silicon",
    arch_intel = "Intel",
    method_msi = "Windows installer (MSI)",
    method_portable = "Windows portable (zip)",
    method_deb = "Debian/Ubuntu package (.deb)",
    method_rpm = "Fedora/RHEL package (.rpm)",
    method_tarball = "Linux tarball (.tar.gz)",
    /// `{reason}`: one of the `reason_*` texts.
    method_unknown = "not recognised ({reason})",
    reason_no_exe = "the program's location is unknown",
    reason_development = "a build from source",
    reason_translocated = "macOS runs it from a temporary copy; move it to the Applications folder",
    reason_appimage = "an AppImage",
    reason_unsupported = "no release is built for this system",
    reason_unrecognized = "not a layout of the release packages",
    /// The install buttons, by method.
    install_restart = "Download and Install",
    install_msi = "Download the Installer",
    install_package = "Download the Package",
    skip = "Skip This Version",
    later = "Later",
    close = "Close",
    cancel = "Cancel",
    no_package = "This installation cannot be updated from here. Download the new version from the release page.",
    no_checksums = "This release has no checksum file, so its download cannot be verified. Download the new version from the release page.",
    /// `{file}`: the package's name.
    downloading = "Downloading {file}…",
    /// `{done}`, `{total}`: sizes.
    progress = "{done} of {total}",
    preparing = "Download verified (SHA-256). Preparing the update…",
    /// `{product}`
    ready_restart = "The update is downloaded and verified. Restarting closes your server connections as Quit does, replaces {product} with the new version and starts it. If the new version does not start, this one is put back.",
    restart_now = "Restart Now",
    /// `{product}`
    ready_installer = "The installer is downloaded and verified. {product} will quit, closing your server connections as Quit does, and Windows Installer will update it.",
    quit_install = "Quit and Install",
    ready_package = "The package is downloaded and verified. Installing it needs administrator rights, so it is not installed for you. Run this command, or open the package with your software installer:",
    copy_command = "Copy Command",
    open_installer = "Open with Software Installer",
    /// `{product}`
    package_restart = "Restart {product} after installing it.",
    /// `{error}`
    failed = "The update failed: {error}",
    /// `{version}`, `{reason}`
    rolled_back = "The update to {version} did not start and was undone: {reason}",
    integrity_note = "Downloads are checked against the SHA-256 checksums published with the release. The packages are not code-signed.",
    /// Options.
    options_heading = "Updates",
    options_check = "Check for updates automatically (at most once a day, from GitHub)",
    options_check_hint = "Turn this off on networks without internet access; Check for Updates… still works when asked.",
}
