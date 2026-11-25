//! Runs the mock XAPI server until Ctrl+C, to try the app without a real host.
//!
//! ```text
//! cargo run -p xapi-mock --example serve                     # HTTPS (self-signed), lab fixture
//! cargo run -p xapi-mock --example serve -- http             # plain HTTP
//! cargo run -p xapi-mock --example serve -- capture          # sanitized real XCP-ng 8.3 host
//! cargo run -p xapi-mock --example serve -- --port 47123     # a fixed port (to restart it)
//! cargo run -p xapi-mock --example serve -- renewed          # present the "renewed" certificate
//! ```

use xapi_mock::{MockCertificate, MockServer};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tls = !args.iter().any(|a| a == "http");
    let records = if args.iter().any(|a| a == "capture") {
        xapi_mock::capture_records()
    } else {
        xapi_mock::fixture_records()
    };
    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .map(|p| p.parse::<u16>().expect("--port takes a port number"));
    let server = match port {
        Some(port) => MockServer::start_on(port, tls, records).await,
        None => MockServer::start_with_records(tls, records).await,
    };
    if args.iter().any(|a| a == "renewed") {
        server.set_certificate(MockCertificate::Renewed);
    }
    println!("mock XAPI server listening on {}", server.address());
    println!(
        "log in as {} / {}",
        xapi_mock::USERNAME,
        xapi_mock::PASSWORD
    );
    tokio::signal::ctrl_c().await.expect("ctrl-c handler");
}
