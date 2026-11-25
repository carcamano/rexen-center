//! Regenerates `crates/xapi/src/generated/` from the pinned datamodel.
//!
//! ```text
//! cargo run -p xapi-codegen              # rewrite the generated files
//! cargo run -p xapi-codegen -- --check   # fail if they are out of date
//! ```

use std::process::ExitCode;

fn main() -> ExitCode {
    let check = std::env::args().skip(1).any(|a| a == "--check");
    let paths = xapi_codegen::Paths::from_manifest_dir();
    match xapi_codegen::run(&paths, check) {
        Ok(files) => {
            let verb = if check { "up to date" } else { "generated" };
            println!("{} files {verb} in {}", files.len(), paths.out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("xapi-codegen: {e}");
            ExitCode::FAILURE
        }
    }
}
