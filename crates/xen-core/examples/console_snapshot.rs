//! View-only console snapshot from a real server: logs in, finds a host's
//! console (its control domain) or a VM's console by exact name, opens it
//! through the XAPI tunnel, watches it for a few seconds **without sending
//! any input** (no keys, pointer or clipboard), writes the screen to a PPM
//! image and logs out.
//!
//! ```text
//! XAPI_HOST=192.0.2.10 XAPI_USER=root XAPI_PASSWORD=… XAPI_FINGERPRINT=AA:BB:… \
//! CONSOLE_HOST=1 CONSOLE_OUT=/tmp/dom0.ppm cargo run -p xen-core --example console_snapshot
//! ```
//!
//! `CONSOLE_VM=<exact name_label>` instead of `CONSOLE_HOST=1` shows a VM
//! (refused unless exactly one VM has that name). `CONSOLE_SECONDS` sets how
//! long to watch (default 5).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use xapi::{ClientOptions, Endpoint, Fingerprint, JsonRpcClient, Session, TrustStore};
use xen_core::console::{self, ConsoleTarget};
use xen_vnc::{Screen, ServerEvent};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = Endpoint::parse(&env("XAPI_HOST"))?;
    let trust = TrustStore::new();
    let fingerprint: Fingerprint = env("XAPI_FINGERPRINT").parse()?;
    trust.trust(&endpoint.authority(), fingerprint);
    let client = Arc::new(JsonRpcClient::new(
        &endpoint,
        &trust,
        &ClientOptions::default(),
    )?);
    let session = Session::login_with_password(
        client,
        &env("XAPI_USER"),
        &env("XAPI_PASSWORD"),
        "xenadmin-rs console_snapshot",
    )
    .await?;
    let result = snapshot(&session, &trust).await;
    let _ = session.logout().await;
    result
}

async fn snapshot(session: &Session, trust: &TrustStore) -> Result<(), Box<dyn std::error::Error>> {
    let (model, _) = xen_core::connection::initial_load(session).await?;
    let target = if std::env::var("CONSOLE_HOST").is_ok() {
        let mut hosts = model.hosts.keys();
        let host = hosts.next().ok_or("no host")?.clone();
        if hosts.next().is_some() {
            return Err("more than one host; this example shows a standalone host only".into());
        }
        ConsoleTarget::Host(host)
    } else {
        let name = env("CONSOLE_VM");
        let matches: Vec<_> = model
            .vms
            .iter()
            .filter(|(_, v)| v.name_label == name && !v.is_a_template && !v.is_control_domain)
            .map(|(r, _)| r.clone())
            .collect();
        if matches.len() != 1 {
            return Err(format!("{} VMs are named {name:?}; refusing", matches.len()).into());
        }
        ConsoleTarget::Vm(matches[0].clone())
    };
    let vm = console::resolve(&model, &target).map_err(|s| format!("no console: {s:?}"))?;
    let locations = console::rfb_consoles(&model, &vm);
    println!("rfb consoles: {}", locations.len());
    let stream = console::open_console(&model, &vm, session, trust).await?;
    println!("tunnel open");

    let screen = Screen::new();
    // Never used to send anything: the console is only watched.
    let (_inputs, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let updates = Arc::new(Mutex::new(0u32));
    let on_event = {
        let updates = updates.clone();
        move |e: ServerEvent| match e {
            ServerEvent::Connected(init) => println!(
                "RFB {}.{}, {}x{}, desktop {:?}",
                init.version.0, init.version.1, init.width, init.height, init.name
            ),
            ServerEvent::Updated => *updates.lock().unwrap() += 1,
            other => println!("{other:?}"),
        }
    };
    let seconds: u64 = std::env::var("CONSOLE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let run = xen_vnc::client::run(stream, &screen, &mut rx, &on_event);
    match tokio::time::timeout(Duration::from_secs(seconds), run).await {
        Err(_) => {}
        Ok(r) => println!("session ended: {r:?}"),
    }
    let state = screen.lock();
    println!(
        "updates: {}, scancodes (QEMU Extended Key Event): {}, cursor: {}",
        updates.lock().unwrap(),
        state.scancodes,
        state.cursor.is_some()
    );
    if let Ok(out) = std::env::var("CONSOLE_OUT") {
        let fb = &state.framebuffer;
        let mut ppm = format!("P6\n{} {}\n255\n", fb.width(), fb.height()).into_bytes();
        for p in fb.rgba().as_chunks::<4>().0 {
            ppm.extend_from_slice(&p[..3]);
        }
        std::fs::write(&out, ppm)?;
        println!("wrote {out}");
    }
    Ok(())
}
