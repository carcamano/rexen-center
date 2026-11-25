//! Runs the mock XAPI server until Ctrl+C, to try the app without a real host.
//!
//! ```text
//! cargo run -p xapi-mock --example serve          # HTTPS (self-signed)
//! cargo run -p xapi-mock --example serve -- http  # plain HTTP
//! ```

#[tokio::main]
async fn main() {
    let tls = std::env::args().nth(1).as_deref() != Some("http");
    let server = xapi_mock::MockServer::start(tls).await;
    println!("mock XAPI server listening on {}", server.address());
    println!(
        "log in as {} / {}",
        xapi_mock::USERNAME,
        xapi_mock::PASSWORD
    );
    tokio::signal::ctrl_c().await.expect("ctrl-c handler");
}
