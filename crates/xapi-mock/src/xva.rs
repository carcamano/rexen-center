//! XVA export and import (F11a): `GET /export` and `PUT /import`.
//!
//! * `GET /export?session_id=…&uuid=…&task_id=…` streams a halted or
//!   suspended VM (or template) as an XVA: `ova.xml` (the VM, its drives,
//!   disks, SRs, interfaces and networks, XML-RPC style), then each disk's
//!   blocks with their checksum files (XXH64 `.xxhash` as XCP-ng 8.3
//!   writes, or SHA-1 `.checksum`), then the end marker. The disks' content
//!   is made up: [`World::blocks`] blocks of [`World::block_size`] bytes
//!   per disk, a pattern derived from the disk's uuid. The body has no
//!   length (it ends when the connection closes, as XAPI's) and goes out
//!   [`World::frame`] bytes at a time; the task's progress follows. The VM
//!   is busy (`export` in its current operations) meanwhile.
//! * `PUT /import?task_id=…&session_id=…&restore=…&force=…&sr_id=…` reads
//!   the body as it arrives: once `ova.xml` is in, the VM, its disks (in
//!   the SR), drives and interfaces exist (the VM's
//!   `other_config["import_task"]` names the task, whose
//!   `other_config["object_creation"]` becomes `"complete"`); networks are
//!   matched by name, then bridge, else made (marked with the task too).
//!   Every block's checksum is checked. The task ends with the VM's
//!   reference as its result (`<value><array><data><value>…`).
//!
//! A cancelled task, a client that goes away, a body that ends early
//! (`IMPORT_ERROR_PREMATURE_EOF`) or a bad checksum
//! (`IMPORT_ERROR_SOME_CHECKSUMS_FAILED`) fail the transfer; a failed
//! import destroys what it made, as XAPI does (unless
//! [`World::keep_half_imported`]). Knobs: [`MockServer::fail_method`] on
//! `http/export` or `http/import` (the server refuses), a link that drops
//! after so many bytes, a wrong block checksum, the pace of the frames.
//! Each disk's content digest is kept ([`MockServer::vdi_digest`]) so a
//! round trip can be checked.
//!
//! [`MockServer::fail_method`]: crate::MockServer::fail_method
//! [`MockServer::vdi_digest`]: crate::MockServer::vdi_digest

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::BodyExt;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode};
use serde_json::{Map, Value, json};

use crate::{Db, MockBody, NULL_REF, State, plain_body, rrd};

/// The transfer knobs and what transfers left.
#[derive(Debug)]
pub(crate) struct World {
    /// Bytes per disk block (XAPI: 1 MiB).
    pub(crate) block_size: usize,
    /// Blocks with content per disk.
    pub(crate) blocks: u64,
    /// Checksums as `.xxhash` (XCP-ng 8.3) rather than `.checksum`.
    pub(crate) xxhash: bool,
    /// Bytes per frame of an export.
    pub(crate) frame: usize,
    /// Pause between two frames (export), or per frame's worth of bytes read
    /// (import).
    pub(crate) delay: Duration,
    /// The link drops after this many bytes of a transfer.
    pub(crate) drop_after: Option<u64>,
    /// An export writes one wrong block checksum.
    pub(crate) corrupt_block: bool,
    /// A failed import leaves its VM (a server that does not clean up).
    pub(crate) keep_half_imported: bool,
    /// Disk → digest of its content (exported or imported).
    pub(crate) digests: HashMap<String, String>,
    /// The largest piece of an import body read at once.
    pub(crate) largest_piece: usize,
}

impl Default for World {
    fn default() -> Self {
        Self {
            block_size: 64 << 10,
            blocks: 4,
            xxhash: true,
            frame: 16 << 10,
            delay: Duration::ZERO,
            drop_after: None,
            corrupt_block: false,
            keep_half_imported: false,
            digests: HashMap::new(),
            largest_piece: 0,
        }
    }
}

// ---- hashes -----------------------------------------------------------------------------

const P1: u64 = 11_400_714_785_074_694_791;
const P2: u64 = 14_029_467_366_897_019_727;
const P3: u64 = 1_609_587_929_392_839_161;
const P4: u64 = 9_650_029_242_287_828_579;
const P5: u64 = 2_870_177_450_012_600_261;

fn round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

/// XXH64 (seed 0) of `data`, upper-case hex as XAPI's `.xxhash` files.
fn xxh64(data: &[u8]) -> String {
    let mut rest = data;
    let mut h = if data.len() >= 32 {
        let mut acc = [P1.wrapping_add(P2), P2, 0, 0u64.wrapping_sub(P1)];
        while rest.len() >= 32 {
            for (i, a) in acc.iter_mut().enumerate() {
                let lane = u64::from_le_bytes(rest[i * 8..i * 8 + 8].try_into().expect("lane"));
                *a = round(*a, lane);
            }
            rest = &rest[32..];
        }
        let mut h = acc[0]
            .rotate_left(1)
            .wrapping_add(acc[1].rotate_left(7))
            .wrapping_add(acc[2].rotate_left(12))
            .wrapping_add(acc[3].rotate_left(18));
        for a in acc {
            h = (h ^ round(0, a)).wrapping_mul(P1).wrapping_add(P4);
        }
        h
    } else {
        P5
    };
    h = h.wrapping_add(data.len() as u64);
    while rest.len() >= 8 {
        let k = round(0, u64::from_le_bytes(rest[..8].try_into().expect("8")));
        h = (h ^ k).rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
        rest = &rest[8..];
    }
    if rest.len() >= 4 {
        let k = u64::from(u32::from_le_bytes(rest[..4].try_into().expect("4")));
        h = (h ^ k.wrapping_mul(P1))
            .rotate_left(23)
            .wrapping_mul(P2)
            .wrapping_add(P3);
        rest = &rest[4..];
    }
    for b in rest {
        h = (h ^ u64::from(*b).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    format!("{:016X}", h ^ (h >> 32))
}

fn sha1(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, data)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The digest of a disk's whole content: the hash of its blocks' hashes.
#[derive(Default)]
struct Digest(Vec<u8>);

impl Digest {
    fn add(&mut self, block: &[u8]) {
        self.0.extend_from_slice(xxh64(block).as_bytes());
    }

    fn finish(&self) -> String {
        xxh64(&self.0)
    }
}

/// Block `index` of the disk with `uuid`: a pattern that differs per disk
/// and per block.
fn block(uuid: &str, index: u64, size: usize) -> Vec<u8> {
    let mut x = uuid.bytes().fold(0x9E37_79B9_7F4A_7C15u64 ^ index, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01B3)
    });
    let mut out = Vec::with_capacity(size + 8);
    while out.len() < size {
        // splitmix64
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(size);
    out
}

// ---- tar ----------------------------------------------------------------------------------

fn tar_header(name: &str, size: u64) -> [u8; 512] {
    let mut h = [0u8; 512];
    let n = name.len().min(100);
    h[..n].copy_from_slice(&name.as_bytes()[..n]);
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"15134030000\0");
    h[156] = b'0';
    h[148..156].copy_from_slice(b"        ");
    let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

fn padding(size: usize) -> usize {
    (512 - size % 512) % 512
}

fn tar_entry(name: &str, content: &[u8]) -> Vec<u8> {
    let mut out = tar_header(name, content.len() as u64).to_vec();
    out.extend_from_slice(content);
    out.resize(out.len() + padding(content.len()), 0);
    out
}

// ---- ova.xml ------------------------------------------------------------------------------

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// A JSON value as XML-RPC (`ids`: references written as export ids).
fn to_xmlrpc(v: &Value, ids: &HashMap<String, String>, out: &mut String) {
    out.push_str("<value>");
    match v {
        Value::Null => {}
        Value::Bool(b) => out.push_str(&format!("<boolean>{}</boolean>", u8::from(*b))),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) if s.starts_with("OpaqueRef:") => {
            out.push_str(ids.get(s).map_or(NULL_REF, String::as_str));
        }
        Value::String(s) => out.push_str(&escape(s)),
        Value::Array(items) => {
            out.push_str("<array><data>");
            for i in items {
                to_xmlrpc(i, ids, out);
            }
            out.push_str("</data></array>");
        }
        Value::Object(m) => {
            out.push_str("<struct>");
            for (k, v) in m {
                out.push_str(&format!("<member><name>{}</name>", escape(k)));
                to_xmlrpc(v, ids, out);
                out.push_str("</member>");
            }
            out.push_str("</struct>");
        }
    }
    out.push_str("</value>");
}

/// A small XML-RPC value parser (enough for `ova.xml`).
struct Parser<'a> {
    s: &'a str,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        self.s = self.s.trim_start();
    }

    fn eat(&mut self, tag: &str) -> bool {
        self.skip_ws();
        match self.s.strip_prefix(tag) {
            Some(rest) => {
                self.s = rest;
                true
            }
            None => false,
        }
    }

    fn text_until(&mut self, end: &str) -> Option<String> {
        let i = self.s.find(end)?;
        let t = unescape(&self.s[..i]);
        self.s = &self.s[i + end.len()..];
        Some(t)
    }

    fn value(&mut self) -> Option<Value> {
        self.skip_ws();
        if self.eat("<value/>") {
            return Some(json!(""));
        }
        if !self.eat("<value>") {
            return None;
        }
        let rest = self.s.trim_start();
        let v = if rest.starts_with("<struct>") {
            self.eat("<struct>");
            let mut m = Map::new();
            while self.eat("<member>") {
                self.eat("<name>");
                let name = self.text_until("</name>")?;
                let v = self.value()?;
                self.eat("</member>");
                m.insert(name, v);
            }
            self.eat("</struct>");
            Value::Object(m)
        } else if rest.starts_with("<array>") {
            self.eat("<array>");
            let mut items = Vec::new();
            if self.eat("<data/>") {
            } else {
                self.eat("<data>");
                while let Some(v) = self.value() {
                    items.push(v);
                }
                self.eat("</data>");
            }
            self.eat("</array>");
            Value::Array(items)
        } else if rest.starts_with("<boolean>") {
            self.eat("<boolean>");
            json!(self.text_until("</boolean>")? == "1")
        } else if let Some(tag) = ["string", "i4", "i8", "int", "double", "dateTime.iso8601"]
            .into_iter()
            .find(|t| rest.starts_with(&format!("<{t}>")))
        {
            self.eat(&format!("<{tag}>"));
            let text = self.text_until(&format!("</{tag}>"))?;
            match tag {
                "i4" | "i8" | "int" => text.parse::<i64>().map_or(json!(text), |n| json!(n)),
                "double" => text.parse::<f64>().map_or(json!(text), |n| json!(n)),
                _ => json!(text),
            }
        } else {
            // A bare string.
            let i = self.s.find("</value>")?;
            let t = unescape(&self.s[..i]);
            self.s = &self.s[i..];
            json!(t)
        };
        if !self.eat("</value>") {
            return None;
        }
        Some(v)
    }
}

fn parse_xmlrpc(xml: &str) -> Option<Value> {
    let start = xml.find("<value")?;
    Parser { s: &xml[start..] }.value()
}

// ---- export -------------------------------------------------------------------------------

/// What an export sends: the metadata, then each disk (export id, uuid).
struct ExportPlan {
    metadata: String,
    disks: Vec<(String, String, String)>,
}

/// The `ova.xml` of `vm` and the disks it carries.
fn plan_export(db: &Db, vm: &str) -> Option<ExportPlan> {
    let vm_rec = db.record("vm", vm)?.clone();
    let mut objects: Vec<(&'static str, String, Value)> = Vec::new();
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut next = 0usize;
    let mut add = |class: &'static str, r: &str, rec: Value, ids: &mut HashMap<String, String>| {
        if ids.contains_key(r) {
            return;
        }
        ids.insert(r.to_owned(), format!("Ref:{next}"));
        next += 1;
        objects.push((class, r.to_owned(), rec));
    };
    add("VM", vm, Value::Object(vm_rec.clone()), &mut ids);
    let list = |rec: &Map<String, Value>, f: &str| -> Vec<String> {
        rec.get(f)
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut disks = Vec::new();
    for vbd in list(&vm_rec, "VBDs") {
        let Some(b) = db.record("vbd", &vbd).cloned() else {
            continue;
        };
        let cd = b.get("type").and_then(Value::as_str) == Some("CD");
        add("VBD", &vbd, Value::Object(b.clone()), &mut ids);
        let vdi = b.get("VDI").and_then(Value::as_str).unwrap_or(NULL_REF);
        if cd || vdi == NULL_REF {
            continue;
        }
        let Some(d) = db.record("vdi", vdi).cloned() else {
            continue;
        };
        if let Some(sr) = d.get("SR").and_then(Value::as_str)
            && let Some(sr_rec) = db.record("sr", sr).cloned()
        {
            add("SR", sr, Value::Object(sr_rec), &mut ids);
        }
        add("VDI", vdi, Value::Object(d.clone()), &mut ids);
        let uuid = d
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        disks.push((ids[vdi].clone(), vdi.to_owned(), uuid));
    }
    for vif in list(&vm_rec, "VIFs") {
        let Some(v) = db.record("vif", &vif).cloned() else {
            continue;
        };
        if let Some(net) = v.get("network").and_then(Value::as_str)
            && let Some(n) = db.record("network", net).cloned()
        {
            add("network", net, Value::Object(n), &mut ids);
        }
        add("VIF", &vif, Value::Object(v), &mut ids);
    }
    let mut xml = String::from(
        "<value><struct><member><name>version</name><value><struct>\
         <member><name>hostname</name><value>mock</value></member>\
         <member><name>date</name><value>2026-11-25</value></member>\
         <member><name>product_version</name><value>8.3.0</value></member>\
         <member><name>product_brand</name><value>XCP-ng</value></member>\
         <member><name>build_number</name><value>mock</value></member>\
         <member><name>xapi_major</name><value><i4>25</i4></value></member>\
         <member><name>xapi_minor</name><value><i4>0</i4></value></member>\
         <member><name>export_vsn</name><value><i4>2</i4></value></member>\
         </struct></value></member><member><name>objects</name><value><array><data>",
    );
    for (class, r, rec) in &objects {
        xml.push_str("<value><struct><member><name>class</name><value>");
        xml.push_str(class);
        xml.push_str("</value></member><member><name>id</name><value>");
        xml.push_str(&ids[r]);
        xml.push_str("</value></member><member><name>snapshot</name>");
        to_xmlrpc(rec, &ids, &mut xml);
        xml.push_str("</member></struct></value>");
    }
    xml.push_str("</data></array></value></member></struct></value>");
    Some(ExportPlan {
        metadata: xml,
        disks,
    })
}

fn query(req: &Request<Incoming>) -> rrd::Query {
    rrd::Query::parse(req.uri().query().unwrap_or_default())
}

/// Checks the session, the task and the RBAC permission of a transfer.
fn check(db: &Db, q: &rrd::Query, permission: &str) -> Result<String, Box<Response<MockBody>>> {
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
    match q.get("task_id") {
        Some(t) if db.objects.contains_key(&("task".to_owned(), t.to_owned())) => Ok(t.to_owned()),
        _ => Err(Box::new(plain_body(
            StatusCode::BAD_REQUEST,
            "task missing",
        ))),
    }
}

/// XAPI's error when the other end of a transfer went away.
const EPIPE: [&str; 2] = [
    "INTERNAL_ERROR",
    "Unix.Unix_error(Unix.EPIPE, \"write\", \"\")",
];

fn task_pending(db: &Db, task: &str) -> bool {
    db.get_str("task", task, "status").as_deref() == Some("pending")
}

/// The export's VM is busy, or free again.
fn set_exporting(db: &mut Db, vm: &str, task: Option<&str>) {
    db.modify("vm", vm, |rec| match task {
        Some(t) => {
            rec.insert("current_operations".into(), json!({ t: "export" }));
            let allowed: Vec<Value> = rec
                .get("allowed_operations")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter(|o| o.as_str() != Some("export"))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            rec.insert("allowed_operations".into(), Value::Array(allowed));
        }
        None => {
            rec.insert("current_operations".into(), json!({}));
            let allowed = crate::provisioning::vm_allowed(rec);
            rec.insert("allowed_operations".into(), json!(allowed));
        }
    });
}

/// `GET /export`.
pub(crate) fn export(state: &Arc<State>, req: &Request<Incoming>) -> Response<MockBody> {
    let q = query(req);
    let mut db = state.db();
    db.calls.push("http/export".to_owned());
    let task = match check(&db, &q, "http/get_export") {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let uuid = q.get("uuid").unwrap_or_default().to_owned();
    let vm = db.objects.iter().find_map(|((c, r), rec)| {
        (c == "vm" && rec.get("uuid").and_then(Value::as_str) == Some(uuid.as_str()))
            .then(|| r.clone())
    });
    let Some(vm) = vm else {
        db.finish_task(&task, "failure", "", &["HANDLE_INVALID", "VM", &uuid]);
        return plain_body(StatusCode::NOT_FOUND, "no such VM");
    };
    if let Some((code, params)) = db.failures.get("http/export").cloned() {
        let mut info = vec![code.as_str()];
        info.extend(params.iter().map(String::as_str));
        db.finish_task(&task, "failure", "", &info);
        return plain_body(StatusCode::INTERNAL_SERVER_ERROR, "export failed");
    }
    let power = db.vm_power(&vm);
    if power != "Halted" && power != "Suspended" {
        db.finish_task(
            &task,
            "failure",
            "",
            &["VM_BAD_POWER_STATE", &vm, "halted", &power.to_lowercase()],
        );
        return plain_body(StatusCode::INTERNAL_SERVER_ERROR, "export failed");
    }
    let Some(plan) = plan_export(&db, &vm) else {
        return plain_body(StatusCode::NOT_FOUND, "no such VM");
    };
    set_exporting(&mut db, &vm, Some(&task));
    let w = &db.xva;
    let (block_size, blocks, xxhash, frame, delay, drop_after, corrupt) = (
        w.block_size,
        w.blocks,
        w.xxhash,
        w.frame.max(1),
        w.delay,
        w.drop_after,
        w.corrupt_block,
    );
    drop(db);
    state.changed.notify_waiters();

    let entry_size = |n: usize| 512 + n + padding(n);
    let sum_size = if xxhash { 16 } else { 40 };
    let total = (entry_size(plan.metadata.len())
        + plan.disks.len() * blocks as usize * (entry_size(block_size) + entry_size(sum_size))
        + 1024) as u64;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let state = state.clone();
    tokio::spawn(async move {
        let mut sent = 0u64;
        // Sends `data` in frames; `Err` once the transfer must stop.
        let mut send = async |data: Vec<u8>| -> Result<(), ()> {
            for piece in data.chunks(frame) {
                // Decided under the lock, acted on after it.
                let stop = {
                    let mut db = state.db();
                    if !task_pending(&db, &task) {
                        // Cancelled (or failed) meanwhile: the link is cut.
                        Some("task cancelled")
                    } else if drop_after.is_some_and(|l| sent + piece.len() as u64 > l) {
                        db.finish_task(&task, "failure", "", &EPIPE);
                        Some("link dropped")
                    } else {
                        let progress = sent as f64 / total as f64;
                        db.modify("task", &task, |r| {
                            r.insert("progress".into(), json!(progress.min(0.99)));
                        });
                        None
                    }
                };
                if let Some(why) = stop {
                    state.changed.notify_waiters();
                    let _ = tx.send(Err(std::io::Error::other(why))).await;
                    return Err(());
                }
                state.changed.notify_waiters();
                if tx.send(Ok(Bytes::copy_from_slice(piece))).await.is_err() {
                    // The client went away.
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
        let mut ok = send(tar_entry("ova.xml", plan.metadata.as_bytes()))
            .await
            .is_ok();
        let mut digests = Vec::new();
        'disks: for (id, vdi, uuid) in &plan.disks {
            let mut digest = Digest::default();
            for i in 0..blocks {
                let data = block(uuid, i, block_size);
                digest.add(&data);
                let name = format!("{id}/{i:08}");
                let mut sum = if xxhash { xxh64(&data) } else { sha1(&data) };
                if corrupt && i == blocks / 2 {
                    sum = sum.chars().rev().collect();
                }
                let ext = if xxhash { "xxhash" } else { "checksum" };
                let mut out = tar_entry(&name, &data);
                out.extend(tar_entry(&format!("{name}.{ext}"), sum.as_bytes()));
                if send(out).await.is_err() {
                    ok = false;
                    break 'disks;
                }
            }
            digests.push((vdi.clone(), digest.finish()));
        }
        if ok {
            ok = send(vec![0u8; 1024]).await.is_ok();
        }
        {
            let mut db = state.db();
            if ok {
                for (vdi, d) in digests {
                    db.xva.digests.insert(vdi, d);
                }
                db.finish_task(&task, "success", "", &[]);
            }
            set_exporting(&mut db, &vm, None);
        }
        state.changed.notify_waiters();
    });
    Response::builder()
        .header("content-type", "application/octet-stream")
        .body(MockBody::Channel(rx))
        .expect("response")
}

// ---- import -------------------------------------------------------------------------------

/// What an import made so far (destroyed if it fails).
#[derive(Default)]
struct Made {
    vm: Option<String>,
    vdis: HashMap<String, String>,
    networks: Vec<String>,
    digests: HashMap<String, Digest>,
}

fn s(rec: &Value, f: &str) -> String {
    match rec.get(f) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn int(rec: &Value, f: &str) -> i64 {
    s(rec, f).parse().unwrap_or(0)
}

fn flag(rec: &Value, f: &str) -> bool {
    match rec.get(f) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(v)) => v == "true" || v == "1",
        _ => false,
    }
}

/// Makes the objects `ova.xml` describes.
fn create_objects(
    db: &mut Db,
    xml: &str,
    sr: &str,
    task: &str,
    made: &mut Made,
) -> Result<(), String> {
    let doc = parse_xmlrpc(xml).ok_or("ova.xml is not XML-RPC")?;
    let objects = doc
        .get("objects")
        .and_then(Value::as_array)
        .ok_or("ova.xml has no objects")?;
    let of = |class: &str| -> Vec<(String, Value)> {
        objects
            .iter()
            .filter(|o| s(o, "class") == class)
            .map(|o| (s(o, "id"), o.get("snapshot").cloned().unwrap_or_default()))
            .collect()
    };
    let (_, vm_rec) = of("VM").into_iter().next().ok_or("no VM in ova.xml")?;
    // Networks: by name, then bridge, else a new one marked with the task.
    let mut networks: HashMap<String, String> = HashMap::new();
    for (id, n) in of("network") {
        let name = s(&n, "name_label");
        let bridge = s(&n, "bridge");
        let found = db
            .objects
            .iter()
            .find(|((c, _), rec)| c == "network" && s(rec, "name_label") == name)
            .or_else(|| {
                db.objects.iter().find(|((c, _), rec)| {
                    c == "network" && !bridge.is_empty() && s(rec, "bridge") == bridge
                })
            })
            .map(|((_, r), _)| r.clone());
        let r = match found {
            Some(r) => r,
            None => {
                let (r, uuid) = db.new_ref("network");
                db.insert(
                    "network",
                    &r,
                    json!({
                        "uuid": uuid, "name_label": name, "name_description": s(&n, "name_description"),
                        "bridge": format!("xapi{}", db.next_object), "MTU": 1500, "VIFs": [], "PIFs": [],
                        "other_config": { "import_task": task }, "managed": true,
                        "allowed_operations": [], "current_operations": {}, "tags": [],
                        "blobs": {}, "default_locking_mode": "unlocked", "assigned_ips": {},
                        "purpose": []
                    }),
                );
                made.networks.push(r.clone());
                r
            }
        };
        networks.insert(id, r);
    }
    // The VM.
    let (vm, uuid) = db.new_ref("vm");
    let mut rec = vm_rec.as_object().cloned().unwrap_or_default();
    let mut other = rec
        .get("other_config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    other.insert("import_task".into(), json!(task));
    for (k, v) in [
        ("uuid", json!(uuid)),
        ("power_state", json!("Halted")),
        ("resident_on", json!(NULL_REF)),
        ("affinity", json!(NULL_REF)),
        ("VBDs", json!([])),
        ("VIFs", json!([])),
        ("VTPMs", json!([])),
        ("consoles", json!([])),
        ("snapshots", json!([])),
        ("snapshot_of", json!(NULL_REF)),
        ("parent", json!(NULL_REF)),
        ("current_operations", json!({})),
        ("other_config", Value::Object(other)),
        ("is_a_template", json!(flag(&vm_rec, "is_a_template"))),
        ("is_a_snapshot", json!(false)),
        ("is_default_template", json!(false)),
        ("is_control_domain", json!(false)),
        (
            "memory_static_max",
            json!(int(&vm_rec, "memory_static_max")),
        ),
        (
            "memory_static_min",
            json!(int(&vm_rec, "memory_static_min")),
        ),
        (
            "memory_dynamic_max",
            json!(int(&vm_rec, "memory_dynamic_max")),
        ),
        (
            "memory_dynamic_min",
            json!(int(&vm_rec, "memory_dynamic_min")),
        ),
        ("VCPUs_max", json!(int(&vm_rec, "VCPUs_max"))),
        ("VCPUs_at_startup", json!(int(&vm_rec, "VCPUs_at_startup"))),
        ("user_version", json!(int(&vm_rec, "user_version").max(1))),
    ] {
        rec.insert(k.into(), v);
    }
    let allowed = crate::provisioning::vm_allowed(&rec);
    rec.insert("allowed_operations".into(), json!(allowed));
    db.insert("vm", &vm, Value::Object(rec));
    made.vm = Some(vm.clone());
    // Disks, in the SR asked for.
    for (id, d) in of("VDI") {
        let r = db.new_vdi(
            sr,
            &s(&d, "name_label"),
            int(&d, "virtual_size"),
            flag(&d, "sharable"),
            flag(&d, "read_only"),
        );
        made.vdis.insert(id.clone(), r);
        made.digests.insert(id, Digest::default());
    }
    for (_, b) in of("VBD") {
        let vdi = made.vdis.get(&s(&b, "VDI")).cloned();
        let kind = if s(&b, "type") == "CD" { "CD" } else { "Disk" };
        let mode = if s(&b, "mode") == "RO" { "RO" } else { "RW" };
        db.new_vbd(
            &vm,
            vdi.as_deref(),
            &s(&b, "userdevice"),
            kind,
            mode,
            flag(&b, "bootable"),
            false,
        );
    }
    for (_, v) in of("VIF") {
        let Some(network) = networks.get(&s(&v, "network")).cloned() else {
            continue;
        };
        let record = json!({
            "VM": vm, "network": network, "device": s(&v, "device"), "MAC": s(&v, "MAC"),
            "MTU": int(&v, "MTU").max(68), "locking_mode": "network_default",
            "ipv4_configuration_mode": "None", "ipv6_configuration_mode": "None",
        });
        if let Err((code, _)) = db.vif_create(Some(&record)) {
            return Err(code);
        }
    }
    db.modify("task", task, |r| {
        let oc = r.entry("other_config").or_insert_with(|| json!({}));
        if let Value::Object(m) = oc {
            m.insert("object_creation".into(), json!("complete"));
        }
    });
    Ok(())
}

/// Destroys what a failed import made.
fn destroy_made(db: &mut Db, made: &Made) {
    if db.xva.keep_half_imported {
        return;
    }
    if let Some(vm) = &made.vm {
        let _ = db.vm_destroy(vm);
    }
    for vdi in made.vdis.values() {
        db.destroy_vdi_record(vdi);
    }
    for net in &made.networks {
        db.remove("network", net);
    }
}

/// The tar stream of an import, read as it arrives.
#[derive(Default)]
struct Reader {
    buf: Vec<u8>,
    /// The entry being read: name and size.
    entry: Option<(String, usize)>,
    zeroes: u8,
    ended: bool,
    /// A block read, waiting for its checksum file.
    pending: Option<(String, Vec<u8>)>,
}

/// Called with each whole entry of an import: its name, content and, for
/// a checksum file, the block it checks.
type OnEntry<'a> = dyn FnMut(&str, &[u8], Option<(String, Vec<u8>)>) -> Result<(), Fail> + 'a;

enum Fail {
    /// An XAPI error for the task.
    Api(&'static str, String),
}

impl Reader {
    /// Takes in a piece of the body; calls `on_entry` for each whole entry.
    fn feed(&mut self, data: &[u8], on_entry: &mut OnEntry<'_>) -> Result<(), Fail> {
        if self.ended {
            return Ok(());
        }
        self.buf.extend_from_slice(data);
        loop {
            match &self.entry {
                None => {
                    if self.buf.len() < 512 {
                        return Ok(());
                    }
                    let h: Vec<u8> = self.buf.drain(..512).collect();
                    if h.iter().all(|b| *b == 0) {
                        self.zeroes += 1;
                        if self.zeroes == 2 {
                            self.ended = true;
                            self.buf.clear();
                            return Ok(());
                        }
                        continue;
                    }
                    let end = h[..100].iter().position(|c| *c == 0).unwrap_or(100);
                    let name = String::from_utf8_lossy(&h[..end]).into_owned();
                    let size_text: String = h[124..136]
                        .iter()
                        .take_while(|b| **b != 0)
                        .map(|b| *b as char)
                        .collect();
                    let size = usize::from_str_radix(size_text.trim(), 8).map_err(|_| {
                        Fail::Api("IMPORT_ERROR", format!("bad tar header for {name}"))
                    })?;
                    self.entry = Some((name, size));
                }
                Some((name, size)) => {
                    let need = size + padding(*size);
                    if self.buf.len() < need {
                        return Ok(());
                    }
                    let (name, size) = (name.clone(), *size);
                    let content: Vec<u8> = self.buf.drain(..need).take(size).collect();
                    self.entry = None;
                    let is_sum = name.ends_with(".xxhash") || name.ends_with(".checksum");
                    if is_sum {
                        let block = self.pending.take();
                        on_entry(&name, &content, block)?;
                    } else if name == "ova.xml" {
                        on_entry(&name, &content, None)?;
                    } else {
                        if let Some((p, _)) = &self.pending {
                            return Err(Fail::Api(
                                "IMPORT_ERROR_SOME_CHECKSUMS_FAILED",
                                format!("block {p} has no checksum"),
                            ));
                        }
                        self.pending = Some((name, content));
                    }
                }
            }
        }
    }
}

/// `PUT /import`. `Err` drops the connection (a broken link).
pub(crate) async fn import(
    state: &Arc<State>,
    req: Request<Incoming>,
) -> Result<Response<MockBody>, std::io::Error> {
    let q = query(&req);
    let (task, sr, delay, drop_after, pace) = {
        let mut db = state.db();
        db.calls.push("http/import".to_owned());
        let task = match check(&db, &q, "http/put_import") {
            Ok(t) => t,
            Err(r) => return Ok(*r),
        };
        let sr = q.get("sr_id").unwrap_or_default().to_owned();
        if !db.objects.contains_key(&("sr".to_owned(), sr.clone())) {
            db.finish_task(&task, "failure", "", &["HANDLE_INVALID", "SR", &sr]);
            return Ok(plain_body(StatusCode::NOT_FOUND, "no such SR"));
        }
        if let Some((code, params)) = db.failures.get("http/import").cloned() {
            let mut info = vec![code.as_str()];
            info.extend(params.iter().map(String::as_str));
            db.finish_task(&task, "failure", "", &info);
            drop(db);
            state.changed.notify_waiters();
            return Ok(plain_body(
                StatusCode::INTERNAL_SERVER_ERROR,
                "import failed",
            ));
        }
        (
            task,
            sr,
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
    let body = req.into_body();
    // XAPI answers at once, then reads the archive; the answer ends (or the
    // connection drops) once the import is over.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);
    let state = state.clone();
    tokio::spawn(async move {
        let read = read_import(&state, &task, &sr, (delay, drop_after, pace), length, body).await;
        if let Err(e) = read {
            let _ = tx.send(Err(e)).await;
        }
    });
    Ok(Response::builder()
        .header("content-type", "application/octet-stream")
        .body(MockBody::Channel(rx))
        .expect("response"))
}

/// Reads the archive of an import as it arrives (see [`import`]); `Err`
/// drops the connection.
async fn read_import(
    state: &Arc<State>,
    task: &str,
    sr: &str,
    (delay, drop_after, pace): (Duration, Option<u64>, usize),
    length: u64,
    mut body: Incoming,
) -> Result<(), std::io::Error> {
    let (task, sr) = (task.to_owned(), sr.to_owned());
    let mut reader = Reader::default();
    let mut made = Made::default();
    let mut received = 0u64;
    let failure: Option<Fail> = loop {
        // A client that went away is noticed by the read failing, or by
        // nothing arriving for a while (as a real server's idle timeout).
        let next = tokio::time::timeout(Duration::from_secs(2), body.frame()).await;
        let Ok(next) = next else {
            break Some(Fail::Api(
                "IMPORT_ERROR_PREMATURE_EOF",
                "the upload stalled".into(),
            ));
        };
        let frame = match next {
            None => break None,
            Some(Ok(f)) => f,
            // The client went away mid-transfer.
            Some(Err(_)) => {
                break Some(Fail::Api(
                    "IMPORT_ERROR_PREMATURE_EOF",
                    "the connection closed".into(),
                ));
            }
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        received += data.len() as u64;
        // Decided under the lock, acted on after it.
        let step = {
            let mut db = state.db();
            db.xva.largest_piece = db.xva.largest_piece.max(data.len());
            if !task_pending(&db, &task) {
                // Cancelled meanwhile.
                destroy_made(&mut db, &made);
                Err("task cancelled")
            } else if drop_after.is_some_and(|l| received > l) {
                db.finish_task(&task, "failure", "", &EPIPE);
                destroy_made(&mut db, &made);
                Err("link dropped")
            } else {
                let progress = received as f64 / length.max(1) as f64;
                db.modify("task", &task, |r| {
                    r.insert("progress".into(), json!(progress.min(0.99)));
                });
                let db = &mut *db;
                let mut on_entry =
                    |name: &str, content: &[u8], block: Option<(String, Vec<u8>)>| {
                        if name == "ova.xml" {
                            let xml = String::from_utf8_lossy(content);
                            return create_objects(&mut *db, &xml, &sr, &task, &mut made)
                                .map_err(|e| Fail::Api("IMPORT_ERROR", e));
                        }
                        let Some((block_name, data)) = block else {
                            return Err(Fail::Api(
                                "IMPORT_ERROR_SOME_CHECKSUMS_FAILED",
                                format!("{name} has no block"),
                            ));
                        };
                        let is_xx = name.ends_with(".xxhash");
                        let ours = if is_xx { xxh64(&data) } else { sha1(&data) };
                        if !ours.eq_ignore_ascii_case(String::from_utf8_lossy(content).trim()) {
                            return Err(Fail::Api(
                                "IMPORT_ERROR_SOME_CHECKSUMS_FAILED",
                                block_name,
                            ));
                        }
                        let id = block_name.split('/').next().unwrap_or_default().to_owned();
                        if let Some(d) = made.digests.get_mut(&id) {
                            d.add(&data);
                        }
                        if let Some(vdi) = made.vdis.get(&id) {
                            let n = data.len() as i64;
                            db.modify("vdi", vdi, |r| {
                                let used = r
                                    .get("physical_utilisation")
                                    .and_then(Value::as_i64)
                                    .unwrap_or(0);
                                r.insert("physical_utilisation".into(), json!(used + n));
                            });
                        }
                        Ok(())
                    };
                Ok(reader.feed(&data, &mut on_entry))
            }
        };
        state.changed.notify_waiters();
        let fed = match step {
            Err(why) => return Err(std::io::Error::other(why)),
            Ok(fed) => fed,
        };
        if let Err(f) = fed {
            break Some(f);
        }
        if !delay.is_zero() {
            // The pace of `frame`-byte pieces, however the bytes arrived.
            let pieces = data.len().div_ceil(pace) as u32;
            tokio::time::sleep(delay * pieces).await;
        }
    };
    let failure = failure.or_else(|| {
        (!reader.ended).then(|| {
            Fail::Api(
                "IMPORT_ERROR_PREMATURE_EOF",
                "the archive ends early".into(),
            )
        })
    });
    let outcome = {
        let mut db = state.db();
        if let Some(Fail::Api(code, why)) = failure {
            if task_pending(&db, &task) {
                db.finish_task(&task, "failure", "", &[code, &why]);
            }
            destroy_made(&mut db, &made);
            Err(std::io::Error::other("import failed"))
        } else if !task_pending(&db, &task) {
            destroy_made(&mut db, &made);
            Err(std::io::Error::other("task cancelled"))
        } else {
            let vm = made.vm.clone().unwrap_or_default();
            for (id, vdi) in &made.vdis {
                if let Some(d) = made.digests.get(id) {
                    db.xva.digests.insert(vdi.clone(), d.finish());
                }
            }
            db.finish_task(
                &task,
                "success",
                &format!("<value><array><data><value>{vm}</value></data></array></value>"),
                &[],
            );
            Ok(())
        }
    };
    state.changed.notify_waiters();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmlrpc_round_trip() {
        let v = json!({"a": "x<y", "b": true, "c": [1, "OpaqueRef:k", "z"], "d": {}});
        let ids = HashMap::from([("OpaqueRef:k".to_owned(), "Ref:3".to_owned())]);
        let mut xml = String::new();
        to_xmlrpc(&v, &ids, &mut xml);
        let back = parse_xmlrpc(&xml).unwrap();
        assert_eq!(
            back,
            json!({"a": "x<y", "b": true, "c": ["1", "Ref:3", "z"], "d": {}})
        );
        assert_eq!(
            parse_xmlrpc(
                "<value><struct><member><name>n</name><value><i4>7</i4></value></member><member><name>e</name><value/></member></struct></value>"
            ),
            Some(json!({"n": 7, "e": ""}))
        );
    }

    #[test]
    fn hashes() {
        assert_eq!(xxh64(b""), "EF46DB3751D8E999");
        assert_eq!(xxh64(b"abc"), "44BC2CF5AD770999");
        assert_eq!(sha1(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_ne!(block("u1", 0, 64), block("u1", 1, 64));
        assert_eq!(block("u1", 2, 100).len(), 100);
    }
}
