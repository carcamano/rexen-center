//! The first start of `rexencenter` copies the settings of the console's
//! former name (`xenadmin-rs`) from the per-user folder, once.
//!
//! Each run gets a scratch `HOME` (the per-user folder lives under it), no
//! configuration-folder override and keyring service names nobody uses, so
//! the owner's real settings and keyring are never read or changed. The
//! command-line mode `--forget-server` stands in for a start: it opens the
//! settings like the window does, without a display.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const OLD_SETTINGS: &str = "[preferences]\ntheme = \"dark\"\n\n[[server]]\naddress = \"192.0.2.10\"\nusername = \"root\"\nreconnect = false\n";
const OLD_PINS: &str = "[servers]\n\"192.0.2.10:443\" = \"AA:BB\"\n";

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "rexencenter-migration-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// The per-user configuration folder of `app` under this home.
    fn config(&self, app: &str) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.0.join("Library/Application Support").join(app)
        } else {
            self.0.join(".config").join(app)
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let id = std::process::id();
        Command::new(env!("CARGO_BIN_EXE_rexencenter"))
            .args(args)
            .env("HOME", &self.0)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("REXENCENTER_CONFIG_DIR")
            .env_remove("XENADMIN_CONFIG_DIR")
            // Service names nobody uses: nothing is saved under them.
            .env(
                "REXENCENTER_KEYRING_SERVICE",
                format!("rexen-center-test-migration-{id}"),
            )
            .env(
                "XENADMIN_KEYRING_SERVICE",
                format!("xenadmin-rs-test-migration-{id}"),
            )
            .env("RUST_LOG", "info")
            .output()
            .unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_first_start_copies_the_former_settings_once() {
    let home = Home::new("once");
    let old = home.config("xenadmin-rs");
    let new = home.config("rexen-center");
    fs::create_dir_all(old.join("logs")).unwrap();
    fs::write(old.join("settings.toml"), OLD_SETTINGS).unwrap();
    fs::write(old.join("known_servers.toml"), OLD_PINS).unwrap();
    fs::write(old.join("mcp.toml"), "[writes]\nenabled = false\n").unwrap();
    fs::write(old.join("logs/xenadmin-rs.2026-11-01.log"), "old").unwrap();

    // First start: migrates, and logs one line saying so.
    let first = home.run(&["--forget-server", "198.51.100.7"]);
    assert_eq!(first.status.code(), Some(1), "{}", stderr(&first));
    let log = stderr(&first);
    assert_eq!(log.matches("migrated the settings of").count(), 1, "{log}");
    assert!(log.contains(&old.display().to_string()), "{log}");
    assert!(log.contains(&new.display().to_string()), "{log}");
    // (The command rewrites settings.toml in full; the values are the old.)
    let carried = read(&new.join("settings.toml"));
    assert!(carried.contains("address = \"192.0.2.10\""), "{carried}");
    assert!(carried.contains("theme = \"dark\""), "{carried}");
    assert_eq!(read(&new.join("known_servers.toml")), OLD_PINS);
    assert!(new.join("mcp.toml").is_file());
    let logs: Vec<String> = fs::read_dir(new.join("logs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!logs.is_empty(), "the new log is written in the new folder");
    assert!(
        logs.iter().all(|f| f.starts_with("rexen-center.")),
        "{logs:?}"
    );

    // The carried settings are the ones in use: forgetting the saved server
    // changes the new folder, never the old one.
    let forget = home.run(&["--forget-server", "192.0.2.10"]);
    assert_eq!(forget.status.code(), Some(0), "{}", stderr(&forget));
    assert!(!read(&new.join("settings.toml")).contains("192.0.2.10"));
    assert_eq!(read(&old.join("settings.toml")), OLD_SETTINGS);
    assert!(old.join("logs/xenadmin-rs.2026-11-01.log").is_file());

    // A later start does not migrate again or bring the old server back.
    let again = home.run(&["--forget-server", "198.51.100.7"]);
    assert!(!stderr(&again).contains("migrated"), "{}", stderr(&again));
    assert!(!read(&new.join("settings.toml")).contains("192.0.2.10"));
}

#[test]
fn an_explicit_folder_is_used_as_is() {
    let home = Home::new("explicit");
    let old = home.config("xenadmin-rs");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("settings.toml"), OLD_SETTINGS).unwrap();
    // The former variable name still selects the folder, so scripts that
    // isolate a profile with it keep doing so.
    let profile = home.0.join("profile");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("settings.toml"), OLD_SETTINGS).unwrap();
    for var in ["REXENCENTER_CONFIG_DIR", "XENADMIN_CONFIG_DIR"] {
        fs::write(profile.join("settings.toml"), OLD_SETTINGS).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rexencenter"));
        let out = cmd
            .args(["--forget-server", "192.0.2.10"])
            .env("HOME", &home.0)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("REXENCENTER_CONFIG_DIR")
            .env_remove("XENADMIN_CONFIG_DIR")
            .env(var, &profile)
            .env(
                "REXENCENTER_KEYRING_SERVICE",
                "rexen-center-test-explicit (unused)",
            )
            .env(
                "XENADMIN_KEYRING_SERVICE",
                "xenadmin-rs-test-explicit (unused)",
            )
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{var}: {}", stderr(&out));
        assert!(!stderr(&out).contains("migrated"), "{var}");
        assert!(!read(&profile.join("settings.toml")).contains("192.0.2.10"));
    }
    assert!(
        !home.config("rexen-center").exists(),
        "no per-user folder made"
    );
    assert_eq!(read(&old.join("settings.toml")), OLD_SETTINGS);
}

#[test]
fn help_names_the_program() {
    let home = Home::new("help");
    let out = home.run(&["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("ReXen Center — XCP-ng Center, born again in Rust"),
        "{text}"
    );
    assert!(text.contains("rexencenter --mcp"), "{text}");
    assert!(!text.to_lowercase().contains("xenadmin"), "{text}");
    assert!(
        !home.config("rexen-center").exists(),
        "--help touches nothing"
    );
}
