//! The streaming transfers of [`xapi::http::HttpTransfer`] against the
//! mock's `/export` and `/import`: a download read piece by piece, an
//! upload read from the file no more than one small buffer at a time, an
//! upload cut short by the file (a short read) or cancelled.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};
use xapi::http::HttpTransfer;
use xapi::{
    ClientOptions, Endpoint, Error, JsonRpcClient, Ref, Session, TaskStatusType, TrustStore,
};
use xapi_mock::MockServer;

/// Far smaller than the transferred files.
const BUFFER: usize = 4096;

async fn login(server: &MockServer) -> Session {
    let endpoint = Endpoint::parse(&server.address()).unwrap();
    let client = Arc::new(
        JsonRpcClient::new(&endpoint, &TrustStore::new(), &ClientOptions::default()).unwrap(),
    );
    Session::login_with_password(client, xapi_mock::USERNAME, xapi_mock::PASSWORD, "t")
        .await
        .unwrap()
}

fn transfer(server: &MockServer) -> HttpTransfer {
    HttpTransfer::new(&server.address(), None, Duration::from_secs(10)).unwrap()
}

/// A reader that records the largest read asked of it.
struct Counting<R> {
    inner: R,
    largest: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}

impl<R: AsyncRead + Unpin> AsyncRead for Counting<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.largest.fetch_max(buf.remaining(), Ordering::SeqCst);
        self.reads.fetch_add(1, Ordering::SeqCst);
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

async fn wait_task(s: &Session, task: &Ref<xapi::Task>) -> TaskStatusType {
    for _ in 0..200 {
        let status = xapi::task::get_status(s, task).await.unwrap();
        if status != TaskStatusType::Pending {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the task did not end")
}

/// Downloads the db VM's export into `path`; returns its size.
async fn download(server: &MockServer, s: &Session, path: &std::path::Path) -> u64 {
    let uuid = xapi::vm::get_uuid(s, &Ref::new("OpaqueRef:vm-db"))
        .await
        .unwrap();
    let task = xapi::task::create(s, "export", "test").await.unwrap();
    let mut d = transfer(server)
        .get(
            "export",
            &[
                ("session_id", s.session_ref().as_str()),
                ("uuid", &uuid),
                ("task_id", task.as_str()),
            ],
        )
        .await
        .unwrap();
    let mut file = tokio::fs::File::create(path).await.unwrap();
    let (mut total, mut pieces, mut largest) = (0u64, 0usize, 0usize);
    while let Some(c) = d.chunk().await.unwrap() {
        file.write_all(&c).await.unwrap();
        total += c.len() as u64;
        pieces += 1;
        largest = largest.max(c.len());
    }
    file.flush().await.unwrap();
    assert_eq!(wait_task(s, &task).await, TaskStatusType::Success);
    // The server sent it in its 16 KiB frames: never whole.
    assert!(
        pieces > 10 && (largest as u64) < total,
        "{pieces} pieces, largest {largest}"
    );
    total
}

fn import_params<'a>(s: &'a Session, task: &'a str) -> [(&'a str, &'a str); 5] {
    [
        ("task_id", task),
        ("session_id", s.session_ref().as_str()),
        ("restore", "false"),
        ("force", "false"),
        ("sr_id", "OpaqueRef:sr-nfs"),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_stream_through_a_small_buffer_both_ways() {
    let server = MockServer::start(false).await;
    server.set_xva_blocks(64 << 10, 8);
    let s = login(&server).await;
    let dir = std::env::temp_dir().join(format!("xapi-transfer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.xva");
    let size = download(&server, &s, &path).await;
    assert!(size > 100 * BUFFER as u64);

    // Upload, counting every read of the file and the progress reported.
    let task = xapi::task::create(&s, "put_import_task", "test")
        .await
        .unwrap();
    let largest = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let reader = Counting {
        inner: tokio::fs::File::open(&path).await.unwrap(),
        largest: largest.clone(),
        reads: reads.clone(),
    };
    let sent = Arc::new(AtomicU64::new(0));
    let seen = sent.clone();
    transfer(&server)
        .put_reader(
            "import",
            &import_params(&s, task.as_str()),
            reader,
            size,
            BUFFER,
            Arc::new(move |n| seen.store(n, Ordering::SeqCst)),
            Arc::new(|| false),
        )
        .await
        .unwrap();
    assert_eq!(wait_task(&s, &task).await, TaskStatusType::Success);
    assert!(largest.load(Ordering::SeqCst) <= BUFFER);
    assert!(reads.load(Ordering::SeqCst) as u64 >= size / BUFFER as u64);
    assert_eq!(sent.load(Ordering::SeqCst), size);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_read_or_a_cancel_aborts_the_upload() {
    let server = MockServer::start(false).await;
    server.set_xva_blocks(64 << 10, 8);
    let s = login(&server).await;
    let dir = std::env::temp_dir().join(format!("xapi-transfer-short-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.xva");
    let size = download(&server, &s, &path).await;

    // The file has fewer bytes than announced.
    let task = xapi::task::create(&s, "put_import_task", "test")
        .await
        .unwrap();
    let file = tokio::fs::File::open(&path).await.unwrap();
    let err = transfer(&server)
        .put_reader(
            "import",
            &import_params(&s, task.as_str()),
            file,
            size + 1000,
            BUFFER,
            Arc::new(|_| {}),
            Arc::new(|| false),
        )
        .await
        .unwrap_err();
    let Error::Io(e) = err else {
        panic!("expected a file error, got {err:?}")
    };
    assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
    assert_eq!(wait_task(&s, &task).await, TaskStatusType::Failure);

    // Cancelled after some pieces.
    let task = xapi::task::create(&s, "put_import_task", "test")
        .await
        .unwrap();
    let file = tokio::fs::File::open(&path).await.unwrap();
    let asked = Arc::new(Mutex::new(0u32));
    let err = transfer(&server)
        .put_reader(
            "import",
            &import_params(&s, task.as_str()),
            file,
            size,
            BUFFER,
            Arc::new(|_| {}),
            Arc::new(move || {
                let mut n = asked.lock().unwrap();
                *n += 1;
                *n > 20
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
    assert_eq!(wait_task(&s, &task).await, TaskStatusType::Failure);
    // Neither left a VM behind.
    let vms = xapi::vm::get_all_records(&s).await.unwrap();
    assert!(
        !vms.values()
            .any(|v| v.other_config.contains_key("import_task")),
        "a half-imported VM is left"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
