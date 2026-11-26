//! Carries the settings of the console's former name (`xenadmin-rs`) over to
//! ReXen Center's on the first start.
//!
//! [`migrate_legacy_settings`] runs only when the new configuration folder
//! does not exist yet and the old one does. It copies the old folder's
//! files (saved servers, pinned certificates, preferences, `mcp.toml` and
//! anything else in it, except the `logs` folder) into a staging folder next
//! to the new one, copies each saved server's password from the old keyring
//! service to the new one (never replacing a password the new service
//! already has), then renames the staging folder into place. Nothing is
//! deleted: the old folder and the old keyring entries stay as they were.
//! Once the new folder exists the function does nothing, so a second start
//! neither migrates again nor overwrites newer settings.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::secrets::{SecretStore, account_for};
use crate::settings::{self, ConfigPaths};

/// The old folder's subfolder that is not carried over (old log files would
/// never be rotated away under the new names).
const SKIPPED_DIR: &str = "logs";

/// What [`migrate_legacy_settings`] carried over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrated {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Files copied.
    pub files: usize,
    /// Saved servers in the carried settings.
    pub servers: usize,
    /// Passwords copied to the new keyring service.
    pub passwords: usize,
    /// Passwords the keyring could not read or write (described, without
    /// the password).
    pub password_errors: Vec<String>,
}

impl std::fmt::Display for Migrated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "migrated the settings of {} to {}: {} files, {} saved servers, {} passwords copied to the new keyring service (the old folder and keyring entries are kept)",
            self.from.display(),
            self.to.display(),
            self.files,
            self.servers,
            self.passwords
        )?;
        if !self.password_errors.is_empty() {
            write!(f, "; {} passwords not copied", self.password_errors.len())?;
        }
        Ok(())
    }
}

/// Copies `old_dir` to `new_dir` and the saved passwords from `old_secrets`
/// to `new_secrets` (see the module docs). `Ok(None)`: nothing to do (the
/// new folder exists, or there is no old one).
pub fn migrate_legacy_settings(
    old_dir: &Path,
    new_dir: &Path,
    old_secrets: &dyn SecretStore,
    new_secrets: &dyn SecretStore,
) -> io::Result<Option<Migrated>> {
    if fs::symlink_metadata(new_dir).is_ok() || !old_dir.is_dir() || old_dir == new_dir {
        return Ok(None);
    }
    let name = new_dir
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no folder name"))?;
    let staging = new_dir.with_file_name(format!(
        ".{}.migrating-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&staging);
    let copied = copy_tree(old_dir, &staging, true).and_then(|files| {
        let carried = carry_passwords(&staging, old_secrets, new_secrets);
        fs::rename(&staging, new_dir).map(|()| (files, carried))
    });
    match copied {
        Ok((files, (servers, passwords, password_errors))) => Ok(Some(Migrated {
            from: old_dir.to_owned(),
            to: new_dir.to_owned(),
            files,
            servers,
            passwords,
            password_errors,
        })),
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            // Another instance finished the migration first.
            if new_dir.is_dir() {
                return Ok(None);
            }
            Err(e)
        }
    }
}

/// Copies the regular files and folders of `from` into `to` (created);
/// symbolic links are skipped. Returns the number of files copied.
fn copy_tree(from: &Path, to: &Path, top: bool) -> io::Result<usize> {
    fs::create_dir_all(to)?;
    let mut files = 0;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            if top && entry.file_name() == SKIPPED_DIR {
                continue;
            }
            files += copy_tree(&entry.path(), &target, false)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
            files += 1;
        }
    }
    Ok(files)
}

/// Copies the password of each server saved in `dir`'s settings; returns
/// (servers, passwords copied, errors).
fn carry_passwords(
    dir: &Path,
    old_secrets: &dyn SecretStore,
    new_secrets: &dyn SecretStore,
) -> (usize, usize, Vec<String>) {
    let servers = match settings::load_settings(&ConfigPaths::at(dir)) {
        Ok(s) => s.servers,
        Err(e) => return (0, 0, vec![format!("settings: {e}")]),
    };
    let mut copied = 0;
    let mut errors = Vec::new();
    for server in &servers {
        let Ok(endpoint) = xapi::Endpoint::parse(&server.address) else {
            continue;
        };
        let account = account_for(&server.username, &endpoint);
        let result = (|| {
            if new_secrets.get(&account)?.is_some() {
                return Ok(false);
            }
            match old_secrets.get(&account)? {
                Some(password) => new_secrets.set(&account, &password).map(|()| true),
                None => Ok(false),
            }
        })();
        match result {
            Ok(true) => copied += 1,
            Ok(false) => {}
            Err(e) => errors.push(format!("{account}: {e}")),
        }
    }
    (servers.len(), copied, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecretStore;

    const OLD_SETTINGS: &str = "[preferences]\ntheme = \"dark\"\n\n[[server]]\naddress = \"192.0.2.10\"\nusername = \"root\"\nreconnect = true\n\n[[server]]\naddress = \"host2:8443\"\nusername = \"admin\"\n";
    const OLD_PINS: &str = "[servers]\n\"192.0.2.10:443\" = \"AA:BB\"\n";

    /// A scratch folder holding `old` and `new` configuration folders.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "xen-core-migration-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn old(&self) -> PathBuf {
            self.0.join("xenadmin-rs")
        }
        fn new_dir(&self) -> PathBuf {
            self.0.join("rexen-center")
        }
        /// A legacy folder with settings, pins, an MCP config and a log.
        fn write_old(&self) {
            let old = self.old();
            fs::create_dir_all(old.join("logs")).unwrap();
            fs::write(old.join("settings.toml"), OLD_SETTINGS).unwrap();
            fs::write(old.join("known_servers.toml"), OLD_PINS).unwrap();
            fs::write(old.join("mcp.toml"), "[writes]\nenabled = false\n").unwrap();
            fs::write(old.join("logs/xenadmin-rs.2026-11-01.log"), "old log").unwrap();
            fs::write(old.join("logs/mcp-audit.jsonl"), "{}\n").unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn old_keyring() -> MemorySecretStore {
        let store = MemorySecretStore::new();
        store.set("root@https://192.0.2.10:443", "pw-root").unwrap();
        store.set("admin@https://host2:8443", "pw-admin").unwrap();
        store.set("other@https://unsaved:443", "pw-other").unwrap();
        store
    }

    #[test]
    fn old_folder_only_is_copied_with_its_passwords() {
        let scratch = Scratch::new("old-only");
        scratch.write_old();
        let (old_keys, new_keys) = (old_keyring(), MemorySecretStore::new());
        let done =
            migrate_legacy_settings(&scratch.old(), &scratch.new_dir(), &old_keys, &new_keys)
                .unwrap()
                .expect("migrated");
        assert_eq!(done.files, 3, "settings, pins and mcp.toml; no logs");
        assert_eq!((done.servers, done.passwords), (2, 2));
        assert!(done.password_errors.is_empty());
        assert!(done.to_string().contains("2 passwords copied"), "{done}");

        let new = scratch.new_dir();
        assert_eq!(
            fs::read_to_string(new.join("settings.toml")).unwrap(),
            OLD_SETTINGS
        );
        assert_eq!(
            fs::read_to_string(new.join("known_servers.toml")).unwrap(),
            OLD_PINS
        );
        assert!(new.join("mcp.toml").is_file());
        assert!(!new.join("logs").exists(), "logs are not carried");
        let settings = settings::load_settings(&ConfigPaths::at(&new)).unwrap();
        assert_eq!(settings.servers.len(), 2);
        // Only the saved servers' passwords move to the new service.
        assert_eq!(
            new_keys.accounts(),
            ["admin@https://host2:8443", "root@https://192.0.2.10:443"]
        );
        assert_eq!(
            new_keys
                .get("root@https://192.0.2.10:443")
                .unwrap()
                .as_deref(),
            Some("pw-root")
        );
        // Copy, not move: the old folder and keyring entries are untouched.
        assert_eq!(old_keys.accounts().len(), 3);
        assert_eq!(
            fs::read_to_string(scratch.old().join("settings.toml")).unwrap(),
            OLD_SETTINGS
        );
        assert!(scratch.old().join("logs/mcp-audit.jsonl").is_file());
        // No staging folder is left behind.
        let leftovers: Vec<_> = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 2, "{leftovers:?}");
    }

    #[test]
    fn a_second_start_does_not_migrate_again() {
        let scratch = Scratch::new("second");
        scratch.write_old();
        let (old_keys, new_keys) = (old_keyring(), MemorySecretStore::new());
        migrate_legacy_settings(&scratch.old(), &scratch.new_dir(), &old_keys, &new_keys)
            .unwrap()
            .expect("first start migrates");
        // The user changes things under the new name.
        let newer = "[preferences]\ntheme = \"light\"\n";
        fs::write(scratch.new_dir().join("settings.toml"), newer).unwrap();
        new_keys
            .set("root@https://192.0.2.10:443", "changed")
            .unwrap();
        fs::write(
            scratch.old().join("settings.toml"),
            OLD_SETTINGS.replace("dark", "system"),
        )
        .unwrap();
        let again =
            migrate_legacy_settings(&scratch.old(), &scratch.new_dir(), &old_keys, &new_keys)
                .unwrap();
        assert_eq!(again, None);
        assert_eq!(
            fs::read_to_string(scratch.new_dir().join("settings.toml")).unwrap(),
            newer
        );
        assert_eq!(
            new_keys
                .get("root@https://192.0.2.10:443")
                .unwrap()
                .as_deref(),
            Some("changed")
        );
    }

    #[test]
    fn both_folders_leave_the_new_one_alone() {
        let scratch = Scratch::new("both");
        scratch.write_old();
        fs::create_dir_all(scratch.new_dir()).unwrap();
        fs::write(scratch.new_dir().join("settings.toml"), "").unwrap();
        let (old_keys, new_keys) = (old_keyring(), MemorySecretStore::new());
        let result =
            migrate_legacy_settings(&scratch.old(), &scratch.new_dir(), &old_keys, &new_keys)
                .unwrap();
        assert_eq!(result, None);
        assert_eq!(
            fs::read_to_string(scratch.new_dir().join("settings.toml")).unwrap(),
            ""
        );
        assert!(!scratch.new_dir().join("known_servers.toml").exists());
        assert!(new_keys.accounts().is_empty());
    }

    #[test]
    fn new_folder_only_or_none_do_nothing() {
        let scratch = Scratch::new("new-only");
        fs::create_dir_all(scratch.new_dir()).unwrap();
        let (old_keys, new_keys) = (old_keyring(), MemorySecretStore::new());
        assert_eq!(
            migrate_legacy_settings(&scratch.old(), &scratch.new_dir(), &old_keys, &new_keys)
                .unwrap(),
            None
        );
        assert!(!scratch.old().exists());

        let none = Scratch::new("none");
        assert_eq!(
            migrate_legacy_settings(&none.old(), &none.new_dir(), &old_keys, &new_keys).unwrap(),
            None
        );
        assert!(!none.new_dir().exists(), "nothing is created");
        assert!(new_keys.accounts().is_empty());
    }

    #[test]
    fn a_password_already_under_the_new_service_is_kept() {
        let scratch = Scratch::new("keep");
        scratch.write_old();
        let (old_keys, new_keys) = (old_keyring(), MemorySecretStore::new());
        new_keys.set("admin@https://host2:8443", "newer").unwrap();
        let done =
            migrate_legacy_settings(&scratch.old(), &scratch.new_dir(), &old_keys, &new_keys)
                .unwrap()
                .unwrap();
        assert_eq!(done.passwords, 1);
        assert_eq!(
            new_keys.get("admin@https://host2:8443").unwrap().as_deref(),
            Some("newer")
        );
    }

    /// A keyring that fails every call.
    struct Broken;

    impl SecretStore for Broken {
        fn get(&self, _: &str) -> Result<Option<String>, crate::secrets::SecretError> {
            Err(crate::secrets::SecretError("locked".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<(), crate::secrets::SecretError> {
            Err(crate::secrets::SecretError("locked".into()))
        }
        fn delete(&self, _: &str) -> Result<(), crate::secrets::SecretError> {
            Ok(())
        }
    }

    #[test]
    fn a_locked_keyring_still_carries_the_files() {
        let scratch = Scratch::new("locked");
        scratch.write_old();
        let done = migrate_legacy_settings(&scratch.old(), &scratch.new_dir(), &Broken, &Broken)
            .unwrap()
            .unwrap();
        assert_eq!((done.files, done.passwords), (3, 0));
        assert_eq!(done.password_errors.len(), 2);
        assert!(!done.password_errors.iter().any(|e| e.contains("pw-")));
        assert!(
            done.to_string().contains("2 passwords not copied"),
            "{done}"
        );
        assert!(scratch.new_dir().join("settings.toml").is_file());
    }
}
