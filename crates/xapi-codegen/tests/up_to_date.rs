//! The committed bindings must match what the generator produces from the
//! pinned datamodel (and the datamodel must match its pinned sha256).

#[test]
fn generated_code_is_up_to_date() {
    let paths = xapi_codegen::Paths::from_manifest_dir();
    if let Err(e) = xapi_codegen::run(&paths, true) {
        panic!("{e}\nrun `cargo run -p xapi-codegen` and commit the result");
    }
}
