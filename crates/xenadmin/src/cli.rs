//! Command-line provisioning of saved servers, without opening the window:
//!
//! ```text
//! xenadmin --add-server <address> [--user <name>] --password-stdin
//! xenadmin --forget-server <address>
//! ```
//!
//! `--add-server` saves the server (marked to reconnect on the next start)
//! and stores the password read from standard input in the OS keyring, like
//! the connect dialog does with "Remember password". The certificate is
//! still confirmed in the window on first connection. `--forget-server`
//! removes the server and its saved password.

use std::io::BufRead;

use xapi::Endpoint;
use xen_core::Store;

/// What the command line asks for.
pub enum Cli {
    /// Open the window.
    Gui,
    AddServer {
        address: String,
        user: String,
    },
    ForgetServer {
        address: String,
    },
    Help,
}

/// Parses the arguments (without the program name).
pub fn parse(args: &[String]) -> Result<Cli, String> {
    let mut iter = args.iter();
    let Some(first) = iter.next() else {
        return Ok(Cli::Gui);
    };
    match first.as_str() {
        "-h" | "--help" => Ok(Cli::Help),
        "--add-server" => {
            let address = iter.next().ok_or("--add-server needs an address")?.clone();
            let mut user = "root".to_owned();
            let mut stdin = false;
            while let Some(arg) = iter.next() {
                match arg.as_str() {
                    "--user" => user = iter.next().ok_or("--user needs a name")?.clone(),
                    "--password-stdin" => stdin = true,
                    other => return Err(format!("unexpected argument {other}")),
                }
            }
            if !stdin {
                return Err(
                    "--add-server reads the password from standard input: add --password-stdin"
                        .into(),
                );
            }
            Ok(Cli::AddServer { address, user })
        }
        "--forget-server" => {
            let address = iter
                .next()
                .ok_or("--forget-server needs an address")?
                .clone();
            match iter.next() {
                Some(other) => Err(format!("unexpected argument {other}")),
                None => Ok(Cli::ForgetServer { address }),
            }
        }
        other => Err(format!("unknown argument {other} (see --help)")),
    }
}

pub const USAGE: &str = "usage:
  xenadmin                                   open the window
  xenadmin --add-server <address> [--user <name>] --password-stdin
                                             save a server and its password (read from stdin)
  xenadmin --forget-server <address>         remove a saved server and its password";

/// Runs a provisioning command; returns the process exit code.
pub fn run(cli: Cli, store: &Store) -> i32 {
    match cli {
        Cli::Gui => 0,
        Cli::Help => {
            println!("{USAGE}");
            0
        }
        Cli::AddServer { address, user } => {
            let endpoint = match Endpoint::parse(&address) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("{e}");
                    return 2;
                }
            };
            let mut password = String::new();
            if let Err(e) = std::io::stdin().lock().read_line(&mut password) {
                eprintln!("could not read the password: {e}");
                return 2;
            }
            let password = password.trim_end_matches(['\r', '\n']);
            if password.is_empty() {
                eprintln!("empty password on standard input");
                return 2;
            }
            if let Err(e) = store.save_password(&user, &endpoint, password) {
                eprintln!("could not save the password in the keyring: {e}");
                return 1;
            }
            let label = endpoint.to_string();
            store.remember_server(None, &label, &user);
            println!("saved {label} (user {user})");
            0
        }
        Cli::ForgetServer { address } => {
            let label = Endpoint::parse(&address).map_or(address, |e| e.to_string());
            let before = store.settings().servers.len();
            let after = store.forget_server(&label).servers.len();
            if after < before {
                println!("forgot {label}");
                0
            } else {
                eprintln!("{label} is not a saved server");
                1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use xen_core::{MemorySecretStore, SecretStore};

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parses_commands() {
        assert!(matches!(parse(&[]), Ok(Cli::Gui)));
        assert!(matches!(
            parse(&args(&["--add-server", "10.0.0.1", "--password-stdin"])),
            Ok(Cli::AddServer { ref user, .. }) if user == "root"
        ));
        assert!(matches!(
            parse(&args(&["--add-server", "h", "--user", "admin", "--password-stdin"])),
            Ok(Cli::AddServer { ref user, .. }) if user == "admin"
        ));
        assert!(
            parse(&args(&["--add-server", "h"])).is_err(),
            "no password source"
        );
        assert!(parse(&args(&["--add-server", "h", "--password", "x"])).is_err());
        assert!(matches!(
            parse(&args(&["--forget-server", "h"])),
            Ok(Cli::ForgetServer { .. })
        ));
        assert!(parse(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn forget_removes_entry_and_password() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = Store::in_memory(secrets.clone());
        let endpoint = Endpoint::parse("10.0.0.1").unwrap();
        store.save_password("root", &endpoint, "pw").unwrap();
        store.remember_server(None, "10.0.0.1", "root");
        let code = run(
            Cli::ForgetServer {
                address: "https://10.0.0.1:443".into(),
            },
            &store,
        );
        assert_eq!(code, 0);
        assert!(store.settings().servers.is_empty());
        assert_eq!(secrets.get("root@https://10.0.0.1:443").unwrap(), None);
        assert_eq!(
            run(
                Cli::ForgetServer {
                    address: "10.0.0.1".into()
                },
                &store
            ),
            1
        );
    }
}
