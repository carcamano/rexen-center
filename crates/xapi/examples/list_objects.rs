//! Read-only smoke check against a real XAPI server: logs in, prints the
//! pool, hosts and guest VMs with their power state, follows `event.from` for
//! a few seconds, then logs out. It never changes anything on the server.
//!
//! ```text
//! XAPI_HOST=10.0.0.5 XAPI_USER=root XAPI_PASSWORD=… cargo run -p xapi --example list_objects
//! ```
//!
//! The first run prints the server certificate fingerprint and stops; check it
//! and re-run with `XAPI_FINGERPRINT=<that value>` to trust it.

use std::sync::Arc;
use std::time::Duration;

use xapi::{ClientOptions, Endpoint, Error, Fingerprint, JsonRpcClient, Session, TrustStore};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

fn parse_fingerprint(s: &str) -> Option<Fingerprint> {
    let bytes: Vec<u8> = s
        .split(':')
        .filter_map(|h| u8::from_str_radix(h, 16).ok())
        .collect();
    Some(Fingerprint(bytes.try_into().ok()?))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let endpoint = Endpoint::parse(&env("XAPI_HOST"))?;
    let trust = TrustStore::new();
    if let Some(fp) = std::env::var("XAPI_FINGERPRINT")
        .ok()
        .as_deref()
        .and_then(parse_fingerprint)
    {
        trust.trust(&endpoint.authority(), fp);
    }
    let client = Arc::new(JsonRpcClient::new(
        &endpoint,
        &trust,
        &ClientOptions::default(),
    )?);
    let session = match Session::login_with_password(
        client,
        &env("XAPI_USER"),
        &env("XAPI_PASSWORD"),
        "xapi-example",
    )
    .await
    {
        Err(Error::UntrustedCertificate { host, fingerprint }) => {
            println!(
                "certificate of {host} is not trusted yet; SHA-256 fingerprint:\n{fingerprint}"
            );
            return Ok(());
        }
        other => other?,
    };

    let pools = xapi::pool::get_all_records(&session).await?;
    let hosts = xapi::host::get_all_records(&session).await?;
    let vms = xapi::vm::get_all_records(&session).await?;
    for pool in pools.values() {
        let master = hosts.get(&pool.master);
        println!(
            "pool {:?}: master {:?}, API {}.{}",
            pool.name_label,
            master.map(|h| h.name_label.as_str()).unwrap_or("?"),
            master.map_or(0, |h| h.api_version_major),
            master.map_or(0, |h| h.api_version_minor),
        );
    }
    for host in hosts.values() {
        println!(
            "host {:?} ({}) {:?}",
            host.name_label,
            host.address,
            host.software_version.get("product_version")
        );
    }
    let templates = vms
        .values()
        .filter(|v| v.is_a_template && !v.is_a_snapshot)
        .count();
    let snapshots = vms.values().filter(|v| v.is_a_snapshot).count();
    let control = vms.values().filter(|v| v.is_control_domain).count();
    println!(
        "VM records: {} (templates {templates}, snapshots {snapshots}, control domains {control})",
        vms.len()
    );
    for vm in vms
        .values()
        .filter(|v| !v.is_a_template && !v.is_a_snapshot && !v.is_control_domain)
    {
        println!(
            "  vm {:?}: {} allowed={:?}",
            vm.name_label,
            vm.power_state,
            vm.allowed_operations
                .iter()
                .map(|o| o.as_str())
                .collect::<Vec<_>>()
        );
    }

    let events_client = Arc::new(JsonRpcClient::new(
        &endpoint,
        &trust,
        &ClientOptions::default(),
    )?);
    let events = session.with_client(events_client);
    let first = events.event_from(&["pool", "host", "vm"], "", 30.0).await?;
    println!("event.from initial batch: {} events", first.events.len());
    let next = tokio::time::timeout(
        Duration::from_secs(8),
        events.event_from(&["pool", "host", "vm"], &first.token, 5.0),
    )
    .await;
    match next {
        Ok(Ok(batch)) => println!("event.from follow-up: {} events", batch.events.len()),
        Ok(Err(e)) => println!("event.from follow-up failed: {e}"),
        Err(_) => println!("event.from follow-up: client timeout"),
    }

    session.logout().await?;
    println!("logged out");
    Ok(())
}
