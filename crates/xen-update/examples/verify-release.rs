//! Checks a release folder as the app's updater would: the signature
//! `SHA256SUMS.minisig` over `SHA256SUMS` against the release keys built
//! into the app, then every file the checksum file lists against it.
//!
//! ```sh
//! cargo run -p xen-update --example verify-release -- <folder>
//! ```
//!
//! The folder is what `packaging/sign-release.sh` signed (or
//! `gh release download v<version> -D <folder>`). Exits non-zero, saying
//! why, when anything is refused.

use std::path::PathBuf;
use std::process::ExitCode;

use xen_update::checksum;
use xen_update::release::CHECKSUMS_ASSET;
use xen_update::signature::{self, SIGNATURE_ASSET};

fn main() -> ExitCode {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!(
            "usage: verify-release <folder with SHA256SUMS, SHA256SUMS.minisig and the packages>"
        );
        return ExitCode::from(2);
    };
    match run(&dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("refused: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(dir: &std::path::Path) -> Result<(), String> {
    let read =
        |name: &str| std::fs::read(dir.join(name)).map_err(|e| format!("cannot read {name}: {e}"));
    let sums = read(CHECKSUMS_ASSET)?;
    let sig = String::from_utf8(read(SIGNATURE_ASSET)?).map_err(|e| e.to_string())?;
    let keys = signature::release_keys();
    let verified = signature::verify(&sums, &sig, &keys).map_err(|e| e.to_string())?;
    println!(
        "{CHECKSUMS_ASSET}: signed by the built-in release key {} (trusted comment: {})",
        verified.key_id, verified.trusted_comment
    );
    let sums = String::from_utf8(sums).map_err(|e| e.to_string())?;
    let mut checked = 0;
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let name = line
            .get(66..)
            .ok_or_else(|| format!("malformed line: {line}"))?;
        let path = dir.join(name);
        if !path.is_file() {
            println!("{name}: not in the folder, not checked");
            continue;
        }
        checksum::verify_file(&path, name, &sums).map_err(|e| e.to_string())?;
        println!("{name}: matches");
        checked += 1;
    }
    println!("{checked} file(s) checked; the updater would accept this release");
    Ok(())
}
