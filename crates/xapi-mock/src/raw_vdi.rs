//! Disk transfers (F11b): `GET /export_raw_vdi` and `PUT /import_raw_vdi`.
//!
//! * `GET /export_raw_vdi?session_id=…&task_id=…&vdi=…&format=vhd|raw`
//!   sends a disk (named by reference or uuid) as a dynamic VHD or as raw
//!   bytes. A disk this mock received keeps the content it received; any
//!   other disk holds made-up content (`vhd::made_up`). The VHD has
//!   no length (it ends when the connection closes, as XAPI's), the raw
//!   bytes have one. A disk attached to a running VM is `VDI_IN_USE`.
//! * `PUT /import_raw_vdi?session_id=…&task_id=…&vdi=…&format=vhd|raw`
//!   writes the body into the disk as it arrives (a dynamic VHD, or raw
//!   bytes from the disk's start); content larger than the disk, or a VHD
//!   that is not one, fails with `VDI_IO_ERROR`. The answer comes once the
//!   body is in. The disk stays whatever happens: the client made it and
//!   removes it.
//!
//! Both follow the task (progress; a cancelled task cuts the transfer) and
//! honour the knobs of the XVA transfers: [`MockServer::fail_method`] on
//! `http/export_raw_vdi` or `http/import_raw_vdi`, a link that drops after
//! so many bytes, the pace of the frames. Each disk's content digest
//! ([`MockServer::vdi_digest`]) is that of its bytes
//! ([`crate::vhd::content_digest`]).
//!
//! [`MockServer::fail_method`]: crate::MockServer::fail_method
//! [`MockServer::vdi_digest`]: crate::MockServer::vdi_digest

use std::sync::Arc;
use std::time::Duration;

use http_body_util::BodyExt;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode};
use serde_json::{Value, json};

use crate::vhd::{self, Content};
use crate::{Db, MockBody, State, plain_body, rrd};

/// XAPI's error when the other end of a transfer went away.
const EPIPE: [&str; 2] = [
    "INTERNAL_ERROR",
    "Unix.Unix_error(Unix.EPIPE, \"write\", \"\")",
];

fn query(req: &Request<Incoming>) -> rrd::Query {
    rrd::Query::parse(req.uri().query().unwrap_or_default())
}

fn task_pending(db: &Db, task: &str) -> bool {
    db.get_str("task", task, "status").as_deref() == Some("pending")
}

/// The disk named by reference or uuid.
fn find_vdi(db: &Db, name: &str) -> Option<String> {
    if db
        .objects
        .contains_key(&("vdi".to_owned(), name.to_owned()))
    {
        return Some(name.to_owned());
    }
    db.objects.iter().find_map(|((c, r), rec)| {
        (c == "vdi" && rec.get("uuid").and_then(Value::as_str) == Some(name)).then(|| r.clone())
    })
}

/// The session, permission, task, disk and format of a transfer.
fn check(
    db: &mut Db,
    q: &rrd::Query,
    permission: &str,
    call: &str,
) -> Result<(String, String, bool), Box<Response<MockBody>>> {
    db.calls.push(call.to_owned());
    let session = q.get("session_id").unwrap_or_default();
    let Some(me) = db.sessions.get(session) else {
        return Err(Box::new(plain_body(
            StatusCode::UNAUTHORIZED,
            "session invalid",
        )));
    };
    if !crate::ops::may(me.role.as_deref(), permission) {
        return Err(Box::new(plain_body(
            StatusCode::FORBIDDEN,
            "RBAC_PERMISSION_DENIED",
        )));
    }
    let task = match q.get("task_id") {
        Some(t) if db.objects.contains_key(&("task".to_owned(), t.to_owned())) => t.to_owned(),
        _ => {
            return Err(Box::new(plain_body(
                StatusCode::BAD_REQUEST,
                "task missing",
            )));
        }
    };
    let name = q.get("vdi").unwrap_or_default().to_owned();
    let Some(vdi) = find_vdi(db, &name) else {
        db.finish_task(&task, "failure", "", &["HANDLE_INVALID", "VDI", &name]);
        return Err(Box::new(plain_body(StatusCode::NOT_FOUND, "no such VDI")));
    };
    let vhd = match q.get("format").unwrap_or("raw") {
        "vhd" => true,
        "raw" => false,
        other => {
            let other = other.to_owned();
            db.finish_task(&task, "failure", "", &["FORMAT_NOT_SUPPORTED", &other]);
            return Err(Box::new(plain_body(
                StatusCode::BAD_REQUEST,
                "format not supported",
            )));
        }
    };
    let failing = call.trim_start_matches("http/");
    if let Some((code, params)) = db.failures.get(&format!("http/{failing}")).cloned() {
        let mut info = vec![code.as_str()];
        info.extend(params.iter().map(String::as_str));
        db.finish_task(&task, "failure", "", &info);
        return Err(Box::new(plain_body(
            StatusCode::INTERNAL_SERVER_ERROR,
            "transfer failed",
        )));
    }
    Ok((task, vdi, vhd))
}

/// A disk attached to a running VM.
fn in_use(db: &Db, vdi: &str) -> bool {
    let vbds = db
        .objects
        .get(&("vdi".to_owned(), vdi.to_owned()))
        .and_then(|r| r.get("VBDs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    vbds.iter().filter_map(Value::as_str).any(|b| {
        let attached = db.get_str("vbd", b, "currently_attached").as_deref() == Some("true");
        let vm = db.get_str("vbd", b, "VM").unwrap_or_default();
        attached || db.get_str("vm", &vm, "power_state").as_deref() == Some("Running")
    })
}

/// `GET /export_raw_vdi`.
pub(crate) fn export(state: &Arc<State>, req: &Request<Incoming>) -> Response<MockBody> {
    let q = query(req);
    let mut db = state.db();
    let (task, vdi, vhd) = match check(
        &mut db,
        &q,
        "http/get_export_raw_vdi",
        "http/export_raw_vdi",
    ) {
        Ok(t) => t,
        Err(r) => {
            drop(db);
            state.changed.notify_waiters();
            return *r;
        }
    };
    if in_use(&db, &vdi) {
        db.finish_task(&task, "failure", "", &["VDI_IN_USE", &vdi, "export"]);
        drop(db);
        state.changed.notify_waiters();
        return plain_body(StatusCode::INTERNAL_SERVER_ERROR, "export failed");
    }
    let size = db
        .objects
        .get(&("vdi".to_owned(), vdi.clone()))
        .and_then(|r| r.get("virtual_size"))
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
        .max(0) as u64;
    let uuid = db.get_str("vdi", &vdi, "uuid").unwrap_or_default();
    let content = db
        .xva
        .contents
        .get(&vdi)
        .cloned()
        .unwrap_or_else(|| vhd::made_up(&uuid, size, db.xva.blocks));
    let (frame, delay, drop_after) = (db.xva.frame.max(1), db.xva.delay, db.xva.drop_after);
    drop(db);
    let digest = vhd::digest(&content);
    let total = if vhd {
        vhd::write_dynamic(size, &content)
            .iter()
            .map(|p| p.len() as u64)
            .sum()
    } else {
        size
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let st = state.clone();
    tokio::spawn(async move {
        let state = st;
        let mut sent = 0u64;
        let mut send = async |data: &[u8]| -> Result<(), ()> {
            for piece in data.chunks(frame) {
                let stop = {
                    let mut db = state.db();
                    if !task_pending(&db, &task) {
                        Some("task cancelled")
                    } else if drop_after.is_some_and(|l| sent + piece.len() as u64 > l) {
                        db.finish_task(&task, "failure", "", &EPIPE);
                        Some("link dropped")
                    } else {
                        let progress = sent as f64 / total.max(1) as f64;
                        db.modify("task", &task, |r| {
                            r.insert("progress".into(), json!(progress.min(0.99)));
                        });
                        None
                    }
                };
                state.changed.notify_waiters();
                if let Some(why) = stop {
                    let _ = tx.send(Err(std::io::Error::other(why))).await;
                    return Err(());
                }
                if tx.send(Ok(Bytes::copy_from_slice(piece))).await.is_err() {
                    {
                        let mut db = state.db();
                        if task_pending(&db, &task) {
                            db.finish_task(&task, "failure", "", &EPIPE);
                        }
                    }
                    state.changed.notify_waiters();
                    return Err(());
                }
                sent += piece.len() as u64;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
            Ok(())
        };
        let mut ok = true;
        if vhd {
            for piece in vhd::write_dynamic(size, &content) {
                if send(&piece).await.is_err() {
                    ok = false;
                    break;
                }
            }
        } else {
            // Raw: every byte, zeros included, one chunk at a time.
            let mut at = 0u64;
            let zeros = vec![0u8; vhd::CHUNK];
            while at < size {
                let n = (size - at).min(vhd::CHUNK as u64) as usize;
                let piece = content.get(&at).map_or(&zeros[..n], |c| &c[..n]);
                if send(piece).await.is_err() {
                    ok = false;
                    break;
                }
                at += n as u64;
            }
        }
        if ok {
            let mut db = state.db();
            db.xva.digests.insert(vdi.clone(), digest);
            db.finish_task(&task, "success", "", &[]);
        }
        state.changed.notify_waiters();
    });
    let mut response = Response::builder().header("content-type", "application/octet-stream");
    if !vhd {
        response = response.header("content-length", size.to_string());
    }
    response.body(MockBody::Channel(rx)).expect("response")
}

/// `PUT /import_raw_vdi`. `Err` drops the connection (a broken link).
pub(crate) async fn import(
    state: &Arc<State>,
    req: Request<Incoming>,
) -> Result<Response<MockBody>, std::io::Error> {
    let q = query(&req);
    let (task, vdi, vhd, capacity, delay, drop_after, pace) = {
        let mut db = state.db();
        let (task, vdi, vhd) = match check(
            &mut db,
            &q,
            "http/put_import_raw_vdi",
            "http/import_raw_vdi",
        ) {
            Ok(t) => t,
            Err(r) => {
                drop(db);
                state.changed.notify_waiters();
                return Ok(*r);
            }
        };
        let capacity = db
            .objects
            .get(&("vdi".to_owned(), vdi.clone()))
            .and_then(|r| r.get("virtual_size"))
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0)
            .max(0) as u64;
        (
            task,
            vdi,
            vhd,
            capacity,
            db.xva.delay,
            db.xva.drop_after,
            db.xva.frame.max(1),
        )
    };
    let length: u64 = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = req.into_body();
    let mut reader = vhd::Reader::new(vhd, capacity);
    let mut received = 0u64;
    let failure: Option<(&str, String)> = loop {
        let next = tokio::time::timeout(Duration::from_secs(2), body.frame()).await;
        let Ok(next) = next else {
            break Some(("VDI_IO_ERROR", "the upload stalled".into()));
        };
        let frame = match next {
            None => break None,
            Some(Ok(f)) => f,
            Some(Err(_)) => break Some(("VDI_IO_ERROR", "the connection closed".into())),
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        received += data.len() as u64;
        let step = {
            let mut db = state.db();
            db.xva.largest_piece = db.xva.largest_piece.max(data.len());
            if !task_pending(&db, &task) {
                Err("task cancelled")
            } else if drop_after.is_some_and(|l| received > l) {
                db.finish_task(&task, "failure", "", &EPIPE);
                Err("link dropped")
            } else {
                let progress = received as f64 / length.max(1) as f64;
                db.modify("task", &task, |r| {
                    r.insert("progress".into(), json!(progress.min(0.99)));
                });
                Ok(())
            }
        };
        state.changed.notify_waiters();
        if let Err(why) = step {
            return Err(std::io::Error::other(why));
        }
        if let Err(e) = reader.feed(&data) {
            break Some(("VDI_IO_ERROR", e.0));
        }
        if !delay.is_zero() {
            let pieces = data.len().div_ceil(pace) as u32;
            tokio::time::sleep(delay * pieces).await;
        }
    };
    let failure = failure.or_else(|| reader.finish().err().map(|e| ("VDI_IO_ERROR", e.0)));
    let outcome = {
        let mut db = state.db();
        let used: u64 = reader.content.len() as u64 * vhd::CHUNK as u64;
        let content: Content = std::mem::take(&mut reader.content);
        let digest = vhd::digest(&content);
        // What arrived is written, whatever happens next.
        db.modify("vdi", &vdi, |r| {
            r.insert("physical_utilisation".into(), json!(used.min(capacity)));
        });
        db.xva.contents.insert(vdi.clone(), content);
        match failure {
            Some((code, why)) => {
                if task_pending(&db, &task) {
                    db.finish_task(&task, "failure", "", &[code, &why]);
                }
                Err(std::io::Error::other("import failed"))
            }
            None if !task_pending(&db, &task) => Err(std::io::Error::other("task cancelled")),
            None => {
                db.xva.digests.insert(vdi.clone(), digest);
                db.finish_task(&task, "success", "", &[]);
                Ok(())
            }
        }
    };
    state.changed.notify_waiters();
    match outcome {
        Ok(()) => Ok(plain_body(StatusCode::OK, "")),
        Err(_) if failure_is_answerable(state, &task) => Ok(plain_body(
            StatusCode::INTERNAL_SERVER_ERROR,
            "import failed",
        )),
        Err(e) => Err(e),
    }
}

/// A failure the server reports with an answer (a refused content) rather
/// than by dropping the connection.
fn failure_is_answerable(state: &Arc<State>, task: &str) -> bool {
    let db = state.db();
    db.objects
        .get(&("task".to_owned(), task.to_owned()))
        .and_then(|r| r.get("error_info"))
        .and_then(Value::as_array)
        .and_then(|e| e.first())
        .and_then(Value::as_str)
        == Some("VDI_IO_ERROR")
}
