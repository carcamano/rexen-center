//! End-to-end tests of the XAPI client against the mock JSON-RPC server.

use std::sync::Arc;
use std::time::Duration;

use xapi::error::codes;
use xapi::{
    ClientOptions, Endpoint, Error, EventOperation, Fingerprint, JsonRpcClient, Ref, Session,
    TrustStore, VmPowerState,
};
use xapi_mock::MockServer;

fn client(server: &MockServer, trust: &TrustStore) -> Arc<JsonRpcClient> {
    let endpoint = Endpoint::parse(&server.address()).unwrap();
    Arc::new(JsonRpcClient::new(&endpoint, trust, &ClientOptions::default()).unwrap())
}

async fn login(server: &MockServer, trust: &TrustStore) -> Session {
    Session::login_with_password(
        client(server, trust),
        xapi_mock::USERNAME,
        xapi_mock::PASSWORD,
        "xapi-tests",
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn untrusted_certificate_is_rejected_with_its_fingerprint_then_trusted() {
    let server = MockServer::start(true).await;
    let trust = TrustStore::new();

    let err = Session::login_with_password(client(&server, &trust), "root", "mock-password", "t")
        .await
        .unwrap_err();
    let Error::UntrustedCertificate { host, fingerprint } = err else {
        panic!("expected untrusted certificate, got {err:?}")
    };
    assert_eq!(host, server.authority());
    assert_eq!(
        fingerprint,
        Fingerprint::of_der(&xapi_mock::certificate_der())
    );
    // Nothing reached the JSON-RPC layer: credentials were never sent.
    assert!(server.calls().is_empty());

    trust.trust(&host, fingerprint);
    let session = login(&server, &trust).await;
    session.logout().await.unwrap();
    assert_eq!(server.active_sessions(), 0);
}

#[tokio::test]
async fn wrong_pin_is_rejected() {
    let server = MockServer::start(true).await;
    let trust = TrustStore::new();
    trust.trust(&server.authority(), Fingerprint([0; 32]));
    let err = Session::login_with_password(client(&server, &trust), "root", "mock-password", "t")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UntrustedCertificate { .. }), "{err:?}");
}

#[tokio::test]
async fn bad_password_is_an_api_error() {
    let server = MockServer::start(false).await;
    let err =
        Session::login_with_password(client(&server, &TrustStore::new()), "root", "nope", "t")
            .await
            .unwrap_err();
    assert!(err.is_api(codes::SESSION_AUTHENTICATION_FAILED), "{err:?}");
}

#[tokio::test]
async fn lists_records() {
    let server = MockServer::start(false).await;
    let session = login(&server, &TrustStore::new()).await;

    let pools = xapi::pool::get_all_records(&session).await.unwrap();
    assert_eq!(pools.len(), 1);
    let pool = pools.values().next().unwrap();
    assert_eq!(pool.name_label, "Lab Pool");

    let hosts = xapi::host::get_all_records(&session).await.unwrap();
    assert_eq!(hosts.len(), 2);
    let master = &hosts[&pool.master];
    assert_eq!(
        (master.api_version_major, master.api_version_minor),
        (2, 21)
    );

    let vms = xapi::vm::get_all_records(&session).await.unwrap();
    assert_eq!(vms.len(), 8);
    let web = &vms[&Ref::new("OpaqueRef:vm-web")];
    assert_eq!(web.power_state, VmPowerState::Running);
    assert_eq!(web.resident_on, pool.master);
    assert!(vms.values().any(|v| v.is_control_domain));
}

#[tokio::test]
async fn start_and_shutdown_emit_events() {
    let server = MockServer::start(false).await;
    let session = login(&server, &TrustStore::new()).await;
    let db = Ref::new("OpaqueRef:vm-db");

    let initial = session.event_from(&["vm"], "", 1.0).await.unwrap();
    assert!(
        initial
            .events
            .iter()
            .all(|e| e.operation == EventOperation::Add)
    );
    assert_eq!(initial.events.len(), 8);

    xapi::vm::start(&session, &db, false, false).await.unwrap();
    let batch = session
        .event_from(&["vm"], &initial.token, 5.0)
        .await
        .unwrap();
    let ev = batch
        .events
        .iter()
        .find(|e| e.opaque_ref == db.as_str())
        .expect("event for db VM");
    assert_eq!(ev.operation, EventOperation::Mod);
    assert_eq!(ev.snapshot.as_ref().unwrap()["power_state"], "Running");

    // Starting again is refused by the server with VM_BAD_POWER_STATE.
    let err = xapi::vm::start(&session, &db, false, false)
        .await
        .unwrap_err();
    assert!(err.is_api(codes::VM_BAD_POWER_STATE), "{err:?}");

    xapi::vm::clean_shutdown(&session, &db).await.unwrap();
    let batch2 = session
        .event_from(&["vm"], &batch.token, 5.0)
        .await
        .unwrap();
    assert!(
        batch2
            .events
            .iter()
            .any(|e| e.snapshot.as_ref().unwrap()["power_state"] == "Halted")
    );
}

#[tokio::test]
async fn event_from_long_polls_until_a_change() {
    let server = MockServer::start(false).await;
    let session = login(&server, &TrustStore::new()).await;
    let initial = session.event_from(&["vm"], "", 1.0).await.unwrap();

    let poll = {
        let session = session.clone();
        let token = initial.token.clone();
        tokio::spawn(async move { session.event_from(&["vm"], &token, 10.0).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !poll.is_finished(),
        "event.from should block while nothing changes"
    );
    server.set_vm_power_state("OpaqueRef:vm-build", "Running");
    let batch = tokio::time::timeout(Duration::from_secs(5), poll)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        batch
            .events
            .iter()
            .any(|e| e.opaque_ref == "OpaqueRef:vm-build")
    );
}

#[tokio::test]
async fn invalid_session_is_reported() {
    let server = MockServer::start(false).await;
    let session = login(&server, &TrustStore::new()).await;
    session.logout().await.unwrap();
    let err = xapi::pool::get_all_records(&session).await.unwrap_err();
    assert!(err.is_api(codes::SESSION_INVALID), "{err:?}");
}
