//! The parsers against sanitized answers of a real XCP-ng 8.3 host
//! (`tests/fixtures/rrd/`), in both formats, and the client against a
//! minimal HTTP server.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xen_rrd::{
    Archives, Cf, Format, Interval, ObjectKind, RrdClient, RrdError, UpdatesQuery, parse_rrd,
    parse_updates,
};

const UPDATES_JSON: &[u8] = include_bytes!("../../../tests/fixtures/rrd/rrd_updates.json");
const UPDATES_XML: &[u8] = include_bytes!("../../../tests/fixtures/rrd/rrd_updates.xml");
const RRD_JSON: &[u8] = include_bytes!("../../../tests/fixtures/rrd/host_rrds.json");
const RRD_XML: &[u8] = include_bytes!("../../../tests/fixtures/rrd/host_rrds.xml");

const HOST: &str = "0b0e0000-0000-4000-8000-000000000001";
const VM: &str = "0b0e0000-0000-4000-8000-0000000000a1";

fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a == b
}

#[test]
fn rrd_updates_read_the_same_in_json_and_xml() {
    let json = parse_updates(UPDATES_JSON).unwrap();
    let xml = parse_updates(UPDATES_XML).unwrap();
    assert_eq!(json.legend, xml.legend);
    assert_eq!(
        (json.start, json.end, json.step),
        (xml.start, xml.end, xml.step)
    );
    assert_eq!(json.step, 5);
    assert_eq!(json.rows.len(), 4);
    assert_eq!(json.legend.len(), 15);
    for (a, b) in json.rows.iter().zip(&xml.rows) {
        assert_eq!(a.t, b.t);
        assert_eq!(a.values.len(), json.legend.len());
        assert!(a.values.iter().zip(&b.values).all(|(x, y)| same(*x, *y)));
    }
    // Newest row first, 5 s apart, as XAPI sends them.
    assert_eq!(json.rows[0].t, json.end);
    assert_eq!(json.rows[0].t - json.rows[1].t, 5);
    // The host's and the VM's columns.
    let host_cpu = json
        .legend
        .iter()
        .position(|s| s.kind == ObjectKind::Host && s.uuid == HOST && s.name == "cpu0")
        .unwrap();
    assert_eq!(json.legend[host_cpu].cf, Cf::Average);
    assert!((0.0..=1.0).contains(&json.rows[0].values[host_cpu]));
    assert!(
        json.legend
            .iter()
            .any(|s| s.kind == ObjectKind::Vm && s.uuid == VM && s.name == "vbd_xvda_read")
    );
    // The gap left in the fixture reads as NaN.
    assert!(json.rows[1].values[2].is_nan());

    let mut archives = Archives::new();
    archives.merge_updates(Interval::FiveSeconds, &json, |s| {
        s.kind == ObjectKind::Vm && s.uuid == VM
    });
    assert_eq!(archives.series(Interval::FiveSeconds, "memory").len(), 4);
    assert!(archives.series(Interval::FiveSeconds, "loadavg").is_empty());
}

#[test]
fn whole_rrds_read_the_same_in_json_and_xml() {
    let json = parse_rrd(RRD_JSON).unwrap();
    let xml = parse_rrd(RRD_XML).unwrap();
    assert_eq!(json.step, 5);
    assert_eq!(xml.step, 5);
    assert_eq!(json.archives.len(), 10);
    assert_eq!(xml.archives.len(), 10);
    let mut names = json.sources.clone();
    names.sort();
    assert_eq!(names, ["cpu0", "memory_free_kib", "memory_total_kib"]);
    let (mut a, mut b) = (Archives::new(), Archives::new());
    a.load_rrd(&json);
    b.load_rrd(&xml);
    for interval in Interval::ALL {
        for name in &names {
            // The two answers were fetched a few seconds apart: same
            // shape, rows ending at each one's last update.
            let step = interval.seconds();
            for (s, rrd) in [
                (a.series(interval, name), &json),
                (b.series(interval, name), &xml),
            ] {
                assert_eq!(s.len(), 3, "{interval:?} {name}");
                assert_eq!(s[2].0 - s[1].0, step);
                assert_eq!(s[1].0 - s[0].0, step);
                let last = rrd.last_update as i64;
                assert!(
                    last - s[2].0 < step && last >= s[2].0,
                    "{interval:?} {name}"
                );
            }
        }
    }
    // The gap left in each fixture.
    assert!(
        a.series(Interval::FiveSeconds, "memory_total_kib")[0]
            .1
            .is_nan()
    );
    assert!(
        b.series(Interval::FiveSeconds, "memory_total_kib")[0]
            .1
            .is_nan()
    );
    // Same values where the hourly archive did not move in between.
    let (ha, hb) = (
        a.series(Interval::OneDay, "memory_total_kib"),
        b.series(Interval::OneDay, "memory_total_kib"),
    );
    assert!(ha.iter().zip(hb).all(|(p, q)| p.0 == q.0 && same(p.1, q.1)));
    let total = a.series(Interval::OneHour, "memory_total_kib")[2].1;
    assert!(total > 1.0e6, "KiB of a real host: {total}");
}

#[test]
fn garbage_is_refused() {
    assert!(matches!(parse_updates(b""), Err(RrdError::Decode(_))));
    assert!(matches!(
        parse_updates(b"{\"meta\":1}"),
        Err(RrdError::Decode(_))
    ));
    assert!(matches!(
        parse_rrd(b"<rrd><step>x</step></rrd>"),
        Err(RrdError::Decode(_))
    ));
    assert!(matches!(parse_rrd(b"not xml"), Err(RrdError::Decode(_))));
}

/// Serves one canned answer and returns the request line it got.
async fn one_shot(body: &'static [u8]) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = s.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n])
            .lines()
            .next()
            .unwrap()
            .to_owned();
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).await.unwrap();
        s.write_all(body).await.unwrap();
        request
    });
    (format!("http://{addr}"), task)
}

#[tokio::test]
async fn the_client_asks_for_json_and_reads_either_format() {
    let query = UpdatesQuery {
        start: 100,
        interval: 5,
        cf: Cf::Average,
        host: false,
        vm_uuid: Some(VM.to_owned()),
    };
    let (base, server) = one_shot(UPDATES_JSON).await;
    let client = RrdClient::new(&base, None, Duration::from_secs(5), "test").unwrap();
    let updates = client.updates("OpaqueRef:s", &query).await.unwrap();
    assert_eq!(updates.rows.len(), 4);
    assert_eq!(client.format(), Some(Format::Json));
    let request = server.await.unwrap();
    assert!(request.starts_with(
        "GET /rrd_updates?session_id=OpaqueRef%3As&start=100&cf=AVERAGE&interval=5&vm_uuid="
    ));
    assert!(request.contains("&json=true"), "{request}");

    // A server without JSON answers in XML: read all the same.
    let (base, server) = one_shot(RRD_XML).await;
    let client = RrdClient::new(&base, None, Duration::from_secs(5), "test").unwrap();
    let rrd = client.host_rrd("OpaqueRef:s").await.unwrap();
    assert_eq!(rrd.archives.len(), 10);
    assert_eq!(client.format(), Some(Format::Xml));
    assert!(
        server
            .await
            .unwrap()
            .starts_with("GET /host_rrds?session_id=")
    );

    // Asking for XML leaves `json` out.
    let (base, server) = one_shot(RRD_XML).await;
    let client = RrdClient::new(&base, None, Duration::from_secs(5), "test")
        .unwrap()
        .prefer_xml();
    client.vm_rrd("OpaqueRef:s", VM).await.unwrap();
    let request = server.await.unwrap();
    assert!(request.starts_with(&format!("GET /vm_rrds?session_id=OpaqueRef%3As&uuid={VM} ")));
}

#[tokio::test]
async fn errors_never_show_the_session() {
    // Nothing listens there.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let client = RrdClient::new(
        &format!("http://{addr}"),
        None,
        Duration::from_secs(2),
        "test",
    )
    .unwrap();
    let err = client
        .host_rrd("OpaqueRef:secret-session")
        .await
        .unwrap_err();
    assert!(matches!(err, RrdError::Transport(_)));
    let text = format!("{err} {err:?}");
    assert!(!text.contains("secret-session"), "{text}");
    // HTTPS needs a TLS configuration.
    assert!(RrdClient::new("https://h", None, Duration::from_secs(1), "t").is_err());
}
