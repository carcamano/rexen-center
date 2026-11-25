//! XenAPI code generator.
//!
//! Reads the pinned datamodel in `data/` (`xenapi.json`, produced by xen-api's
//! `gen_json`, plus `async-messages.json`, the `Async.*` flags `gen_json` does
//! not export) and writes `crates/xapi/src/generated/`:
//!
//! * `classes.rs`: one marker type per class, the `XapiClass`/`XapiObject`
//!   impls and the `for_each_cached_class!` macro;
//! * `enums.rs`: every enum, through `xapi_enum!` (tolerates unknown values);
//! * `records.rs`: one `*Record` struct per class with fields;
//! * `<class>.rs`: one `async fn` per message, plus an `async_*` variant
//!   (returning a task reference) for messages that have one.
//!
//! Messages whose lifecycle state is `Removed_s` are left out (record fields
//! are kept, see [`generate`]'s `live_fields`). The generated code
//! only refers to the hand-written core of the `xapi` crate through `crate::`
//! paths: `Session`, `JsonRpcClient`, `Ref`, `DateTime`, `EventBatch`,
//! `Result`, the `de` (lenient deserializers) and `params` helpers.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

/// Where the pinned datamodel comes from. Update together with `data/`.
pub const DATAMODEL_SOURCE: &str = "xen-api v25.27.0 (commit c7592d75cd92b21f9b618840ba814d15f2fa4325), ocaml/idl/json_backend/gen_json";

/// Classes never kept in the client-side cache: sessions (they hold live
/// session references), the deprecated `user` class and secrets (their
/// values are credentials). The original client ignores them too.
pub const NOT_CACHED: &[&str] = &["session", "user", "secret"];

const REMOVED: &str = "Removed_s";

/// One generated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    pub name: String,
    pub contents: String,
}

/// Paths used by [`run`].
#[derive(Debug, Clone)]
pub struct Paths {
    /// `crates/xapi-codegen/data`.
    pub data: PathBuf,
    /// `crates/xapi/src/generated`.
    pub out: PathBuf,
    /// Directory `rustfmt` runs in (the workspace root, for its config).
    pub workspace: PathBuf,
}

impl Paths {
    /// Paths relative to this crate's manifest directory.
    pub fn from_manifest_dir() -> Self {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        Self {
            data: manifest.join("data"),
            out: manifest.join("../xapi/src/generated"),
            workspace: manifest.join("../.."),
        }
    }
}

/// Regenerates the bindings. With `check`, writes nothing and fails if the
/// committed files differ from what would be generated.
pub fn run(paths: &Paths, check: bool) -> Result<Vec<String>, String> {
    let xenapi = read_verified(&paths.data, "xenapi.json")?;
    let async_msgs = read_verified(&paths.data, "async-messages.json")?;
    let sha = sha256_hex(xenapi.as_bytes());
    let files = generate(&xenapi, &async_msgs, &sha)?;
    let mut problems = Vec::new();
    let mut expected = HashSet::new();
    for file in &files {
        expected.insert(file.name.clone());
        let formatted = rustfmt(&file.contents, &paths.workspace)?;
        let path = paths.out.join(&file.name);
        let current = std::fs::read_to_string(&path).ok();
        if current.as_deref() == Some(formatted.as_str()) {
            continue;
        }
        if check {
            problems.push(format!("{} is out of date", path.display()));
        } else {
            std::fs::create_dir_all(&paths.out).map_err(|e| e.to_string())?;
            std::fs::write(&path, formatted).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let entries = std::fs::read_dir(&paths.out).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".rs") && !expected.contains(&name) {
            if check {
                problems.push(format!("{name} is not generated any more"));
            } else {
                std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
    }
    if check && !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    Ok(files.into_iter().map(|f| f.name).collect())
}

/// Reads `data/<name>` and checks it against `data/SHA256SUMS`.
fn read_verified(dir: &Path, name: &str) -> Result<String, String> {
    let sums = std::fs::read_to_string(dir.join("SHA256SUMS"))
        .map_err(|e| format!("reading SHA256SUMS: {e}"))?;
    let expected = sums
        .lines()
        .filter_map(|l| l.split_once("  "))
        .find(|(_, file)| *file == name)
        .map(|(sum, _)| sum.to_owned())
        .ok_or_else(|| format!("{name} is not listed in SHA256SUMS"))?;
    let bytes = std::fs::read(dir.join(name)).map_err(|e| format!("reading {name}: {e}"))?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(format!(
            "{name}: sha256 {actual} does not match the pinned {expected}"
        ));
    }
    String::from_utf8(bytes).map_err(|e| format!("{name}: {e}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn rustfmt(source: &str, workspace: &Path) -> Result<String, String> {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running rustfmt: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("rustfmt stdin")?;
    let input = source.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    writer
        .join()
        .map_err(|_| "rustfmt writer panicked")?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Datamodel

#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Str,
    Int,
    Float,
    Bool,
    DateTime,
    Ref(String),
    Record(String),
    AnyRecord,
    Enum(String),
    Set(Box<Ty>),
    Option(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    EventBatch,
}

#[derive(Debug)]
struct Class {
    wire: String,
    description: String,
    state: String,
    fields: Vec<Field>,
    messages: Vec<Message>,
}

#[derive(Debug)]
struct Field {
    wire: String,
    description: String,
    ty: Ty,
    state: String,
}

#[derive(Debug)]
struct Message {
    wire: String,
    description: String,
    params: Vec<Param>,
    /// `None` for `void`.
    result: Option<Ty>,
    errors: Vec<String>,
    state: String,
    has_session: bool,
    is_async: bool,
}

#[derive(Debug)]
struct Param {
    wire: String,
    ty: Ty,
    required: bool,
}

/// Enum name -> values (wire value, doc), merged across classes.
type EnumDefs = BTreeMap<String, Vec<(String, String)>>;

fn parse_ty(s: &str) -> Result<Ty, String> {
    let s = s.trim();
    Ok(match s {
        "string" => Ty::Str,
        "int" => Ty::Int,
        "float" => Ty::Float,
        "bool" => Ty::Bool,
        "datetime" => Ty::DateTime,
        "<class> record" => Ty::AnyRecord,
        "an event batch" => Ty::EventBatch,
        _ => {
            if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(") map")) {
                let split = top_level_arrow(inner).ok_or_else(|| format!("bad map type {s}"))?;
                Ty::Map(
                    Box::new(parse_ty(&inner[..split])?),
                    Box::new(parse_ty(&inner[split + 4..])?),
                )
            } else if let Some(inner) = s.strip_suffix(" option") {
                Ty::Option(Box::new(parse_ty(inner)?))
            } else if let Some(inner) = s.strip_suffix(" set") {
                Ty::Set(Box::new(parse_ty(inner)?))
            } else if let Some(name) = s.strip_prefix("enum ") {
                Ty::Enum(name.to_owned())
            } else if let Some(class) = s.strip_suffix(" ref") {
                Ty::Ref(class.to_owned())
            } else if let Some(class) = s.strip_suffix(" record") {
                Ty::Record(class.to_owned())
            } else {
                return Err(format!("unknown type {s:?}"));
            }
        }
    })
}

/// Byte offset of the ` -> ` separating a map's key and value types.
fn top_level_arrow(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' if depth == 0 && s[i..].starts_with(" -> ") => return Some(i),
            _ => {}
        }
    }
    None
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn state_of(v: &Value) -> String {
    v.get("lifecycle")
        .map(|l| str_of(l, "state").to_owned())
        .unwrap_or_default()
}

fn parse_model(xenapi: &str, async_msgs: &str) -> Result<(Vec<Class>, EnumDefs), String> {
    let root: Value = serde_json::from_str(xenapi).map_err(|e| format!("xenapi.json: {e}"))?;
    let async_root: Value =
        serde_json::from_str(async_msgs).map_err(|e| format!("async-messages.json: {e}"))?;
    let classes_json = root.as_array().ok_or("xenapi.json is not an array")?;
    let mut classes = Vec::new();
    // Enum name -> values (value, doc); merged across classes.
    let mut enums = EnumDefs::new();
    for c in classes_json {
        let wire = str_of(c, "name").to_owned();
        let async_set: HashSet<&str> = async_root
            .get(&wire)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        for f in c["fields"].as_array().ok_or("fields")? {
            fields.push(Field {
                wire: str_of(f, "name").to_owned(),
                description: str_of(f, "description").to_owned(),
                ty: parse_ty(str_of(f, "type"))?,
                state: state_of(f),
            });
        }
        let mut messages = Vec::new();
        for m in c["messages"].as_array().ok_or("messages")? {
            let name = str_of(m, "name").to_owned();
            let mut params = Vec::new();
            let mut has_session = false;
            for (i, p) in m["params"].as_array().ok_or("params")?.iter().enumerate() {
                if i == 0 && str_of(p, "name") == "session_id" && str_of(p, "type") == "session ref"
                {
                    has_session = true;
                    continue;
                }
                params.push(Param {
                    wire: str_of(p, "name").to_owned(),
                    ty: parse_ty(str_of(p, "type"))?,
                    required: p.get("required").and_then(Value::as_bool).unwrap_or(true),
                });
            }
            let result_ty = m["result"]
                .as_array()
                .and_then(|r| r.first())
                .and_then(Value::as_str)
                .unwrap_or("void");
            let result = if result_ty == "void" {
                None
            } else {
                Some(parse_ty(result_ty)?)
            };
            let errors = m["errors"]
                .as_array()
                .map(|errs| {
                    errs.iter()
                        .map(|e| str_of(e, "name").to_owned())
                        .filter(|e| !e.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            messages.push(Message {
                is_async: async_set.contains(name.as_str()),
                wire: name,
                description: str_of(m, "description").to_owned(),
                params,
                result,
                errors,
                state: state_of(m),
                has_session,
            });
        }
        for e in c["enums"].as_array().ok_or("enums")? {
            let name = str_of(e, "name").to_owned();
            let values = enums.entry(name).or_default();
            for v in e["values"].as_array().ok_or("enum values")? {
                let value = str_of(v, "name").to_owned();
                if !values.iter().any(|(existing, _)| *existing == value) {
                    values.push((value, str_of(v, "doc").to_owned()));
                }
            }
        }
        classes.push(Class {
            wire,
            description: str_of(c, "description").to_owned(),
            state: state_of(c),
            fields,
            messages,
        });
    }
    Ok((classes, enums))
}

// ---------------------------------------------------------------------------
// Naming

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "extern", "false",
    "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "static", "struct", "trait", "true", "try", "type", "unsafe", "use", "where",
    "while", "abstract", "become", "box", "do", "final", "macro", "override", "priv", "typeof",
    "unsized", "virtual", "yield",
];

/// Snake-case Rust identifier for a wire name (`resident_VMs` -> `resident_vms`).
fn snake(wire: &str) -> String {
    let lower: String = wire
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    match lower.as_str() {
        "self" | "super" | "crate" => format!("{lower}_"),
        _ if KEYWORDS.contains(&lower.as_str()) => format!("r#{lower}"),
        _ => lower,
    }
}

/// `snake` without a raw-identifier prefix (for composing names).
fn bare(ident: &str) -> &str {
    ident.strip_prefix("r#").unwrap_or(ident)
}

/// CamelCase identifier (`VM_guest_metrics` -> `VmGuestMetrics`,
/// `changing_VCPUs` -> `ChangingVcpus`).
fn camel(wire: &str) -> String {
    let mut out = String::new();
    for part in wire.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars.map(|c| c.to_ascii_lowercase()));
        }
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'V');
    }
    out
}

fn class_ident(wire: &str) -> String {
    match wire {
        // `Session` and `Event` are hand-written core types.
        "session" => "SessionClass".to_owned(),
        "event" => "EventClass".to_owned(),
        _ => camel(wire),
    }
}

fn record_ident(wire: &str) -> String {
    format!("{}Record", camel(wire))
}

fn module_ident(wire: &str) -> String {
    snake(wire)
}

/// Plural snake-case name used for the cache's per-class maps.
fn plural(wire: &str) -> String {
    let s = bare(&snake(wire)).to_owned();
    if s.ends_with('s') {
        s
    } else if s.ends_with("ch") || s.ends_with("sh") || s.ends_with('x') {
        format!("{s}es")
    } else if s.ends_with('y') && !s.ends_with("ay") && !s.ends_with("ey") && !s.ends_with("oy") {
        format!("{}ies", &s[..s.len() - 1])
    } else {
        format!("{s}s")
    }
}

/// Names the hand-written core already uses at the crate root.
const CORE_TYPES: &[&str] = &[
    "ClientOptions",
    "DateTime",
    "Endpoint",
    "Error",
    "Event",
    "EventBatch",
    "Fingerprint",
    "JsonRpcClient",
    "Ref",
    "Result",
    "Session",
    "TrustStore",
    "XapiClass",
    "XapiObject",
];
const CORE_MODULES: &[&str] = &[
    "class",
    "datetime",
    "de",
    "endpoint",
    "error",
    "event_batch",
    "generated",
    "macros",
    "params",
    "refs",
    "session_handle",
    "tls",
    "transport",
];

// ---------------------------------------------------------------------------
// Types

struct Names {
    classes: HashMap<String, String>,
    records: HashSet<String>,
    enums: HashMap<String, String>,
}

impl Names {
    fn class(&self, wire: &str) -> Result<String, String> {
        self.classes
            .get(wire)
            .cloned()
            .ok_or_else(|| format!("unknown class {wire}"))
    }

    fn owned(&self, ty: &Ty) -> Result<String, String> {
        Ok(match ty {
            Ty::Str => "String".to_owned(),
            Ty::Int => "i64".to_owned(),
            Ty::Float => "f64".to_owned(),
            Ty::Bool => "bool".to_owned(),
            Ty::DateTime => "crate::DateTime".to_owned(),
            Ty::Ref(c) => format!("crate::Ref<crate::{}>", self.class(c)?),
            Ty::Record(c) => {
                let name = record_ident(c);
                if !self.records.contains(&name) {
                    return Err(format!("record of class {c} without fields"));
                }
                format!("crate::{name}")
            }
            Ty::AnyRecord => "::serde_json::Value".to_owned(),
            Ty::Enum(e) => format!(
                "crate::{}",
                self.enums
                    .get(e)
                    .ok_or_else(|| format!("unknown enum {e}"))?
            ),
            Ty::Set(t) => format!("Vec<{}>", self.owned(t)?),
            Ty::Option(t) => format!("Option<{}>", self.owned(t)?),
            Ty::Map(k, v) => format!(
                "::std::collections::HashMap<{}, {}>",
                self.owned(k)?,
                self.owned(v)?
            ),
            Ty::EventBatch => "crate::EventBatch".to_owned(),
        })
    }

    /// Parameter type: borrowed for anything that is not `Copy`.
    fn param(&self, ty: &Ty) -> Result<String, String> {
        Ok(match ty {
            Ty::Str => "&str".to_owned(),
            Ty::Int | Ty::Float | Ty::Bool => self.owned(ty)?,
            Ty::Set(t) => format!("&[{}]", self.owned(t)?),
            _ => format!("&{}", self.owned(ty)?),
        })
    }
}

/// Whether a parameter is passed by value (so serializing it needs `&`).
fn by_value(ty: &Ty) -> bool {
    matches!(ty, Ty::Int | Ty::Float | Ty::Bool)
}

fn deserialize_with(ty: &Ty) -> &'static str {
    match ty {
        Ty::Str => "crate::de::string",
        Ty::Int => "crate::de::int",
        Ty::Float => "crate::de::float",
        Ty::Bool => "crate::de::bool",
        Ty::Option(_) => "crate::de::option",
        _ => "crate::de::or_default",
    }
}

// ---------------------------------------------------------------------------
// Docs

fn escape_doc(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let line = line.trim();
    if line.starts_with('#') {
        // A leading `#` would make a heading.
        out.push('\\');
    }
    for c in line.chars() {
        match c {
            '[' | ']' | '<' | '>' | '\\' | '*' | '_' | '|' => {
                out.push('\\');
                out.push(c);
            }
            '`' => out.push('\''),
            '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn write_doc(out: &mut String, text: &str) {
    for line in text.trim().lines() {
        let line = escape_doc(line);
        if line.is_empty() {
            out.push_str("///\n");
        } else {
            let _ = writeln!(out, "/// {line}");
        }
    }
}

fn lifecycle_note(state: &str) -> Option<&'static str> {
    match state {
        "Deprecated_s" => Some("**Deprecated** in the XenAPI."),
        "Prototyped_s" => Some("**Prototype**: may change in future XenAPI releases."),
        "Unreleased_s" => Some("**Unreleased**: not in any XenAPI release yet."),
        REMOVED => Some("**Removed** from the XenAPI; servers may still send it."),
        _ => None,
    }
}

fn header(sha: &str) -> String {
    format!(
        "// @generated by xapi-codegen from crates/xapi-codegen/data/xenapi.json\n\
         // ({DATAMODEL_SOURCE}; sha256 {sha}).\n\
         // Do not edit by hand: run `cargo run -p xapi-codegen`.\n\n"
    )
}

// ---------------------------------------------------------------------------
// Generation

/// Generates the unformatted contents of every file of `src/generated/`.
pub fn generate(xenapi: &str, async_msgs: &str, sha: &str) -> Result<Vec<OutputFile>, String> {
    let (classes, enums) = parse_model(xenapi, async_msgs)?;

    // Root namespace bookkeeping: every generated type and module name must be unique.
    let mut taken: HashSet<String> = CORE_TYPES
        .iter()
        .chain(CORE_MODULES)
        .map(|s| (*s).to_owned())
        .collect();
    let mut claim = |name: &str| -> Result<(), String> {
        if taken.insert(name.to_owned()) {
            Ok(())
        } else {
            Err(format!("name clash on {name}"))
        }
    };

    let mut names = Names {
        classes: HashMap::new(),
        records: HashSet::new(),
        enums: HashMap::new(),
    };
    for c in &classes {
        let ident = class_ident(&c.wire);
        claim(&ident)?;
        names.classes.insert(c.wire.clone(), ident);
        if live_fields(c).next().is_some() {
            let record = record_ident(&c.wire);
            claim(&record)?;
            names.records.insert(record);
        }
        if live_messages(c).next().is_some() {
            claim(&module_ident(&c.wire))?;
        }
    }
    for name in enums.keys() {
        let ident = camel(name);
        claim(&ident)?;
        names.enums.insert(name.clone(), ident);
    }

    let mut files = vec![
        OutputFile {
            name: "enums.rs".to_owned(),
            contents: gen_enums(&enums, &names, sha),
        },
        OutputFile {
            name: "records.rs".to_owned(),
            contents: gen_records(&classes, &names, sha)?,
        },
        OutputFile {
            name: "classes.rs".to_owned(),
            contents: gen_classes(&classes, &names, sha)?,
        },
    ];
    let mut modules = Vec::new();
    for c in &classes {
        if live_messages(c).next().is_none() {
            continue;
        }
        let module = module_ident(&c.wire);
        files.push(OutputFile {
            name: format!("{}.rs", bare(&module)),
            contents: gen_calls(c, &names, sha)?,
        });
        modules.push(module);
    }
    files.push(OutputFile {
        name: "mod.rs".to_owned(),
        contents: gen_mod(&modules, sha),
    });
    Ok(files)
}

/// Record fields. Fields marked `Removed_s` are kept: servers still send
/// several of them (`VM.is_snapshot_from_vmpp`, `VIF.metrics`, …) and the
/// original client reads some; decoding tolerates their absence anyway.
fn live_fields(c: &Class) -> impl Iterator<Item = &Field> {
    c.fields.iter()
}

fn live_messages(c: &Class) -> impl Iterator<Item = &Message> {
    c.messages.iter().filter(|m| m.state != REMOVED)
}

fn gen_mod(modules: &[String], sha: &str) -> String {
    let mut out = header(sha);
    out.push_str(
        "//! XenAPI classes, records, enums and calls, generated from the pinned\n\
         //! datamodel (see `crates/xapi-codegen`).\n\
         //!\n\
         //! * one marker type per class (`Vm`, `Host`, …) used in [`crate::Ref`],\n\
         //!   with [`crate::XapiClass`] (and [`crate::XapiObject`] when it has a record);\n\
         //! * one `*Record` struct per class: `#[serde(default)]`, lenient scalar\n\
         //!   decoding, unknown fields ignored;\n\
         //! * enums declared with [`crate::xapi_enum!`] (unknown values are kept);\n\
         //! * one module per class with an `async fn` per message taking\n\
         //!   `&Session`, plus `async_*` variants returning a task reference.\n\n\
         #![allow(clippy::all, rustdoc::bare_urls)]\n\n\
         mod classes;\nmod enums;\nmod records;\n\n",
    );
    for m in modules {
        let _ = writeln!(out, "pub mod {m};");
    }
    let _ = write!(
        out,
        "\npub use classes::*;\npub use enums::*;\npub use records::*;\n\n\
         /// Where the datamodel these bindings were generated from comes from.\n\
         pub const DATAMODEL_SOURCE: &str = {DATAMODEL_SOURCE:?};\n\
         /// SHA-256 of the `xenapi.json` these bindings were generated from.\n\
         pub const DATAMODEL_SHA256: &str = {sha:?};\n"
    );
    out
}

fn gen_enums(enums: &EnumDefs, names: &Names, sha: &str) -> String {
    let mut out = header(sha);
    out.push_str("//! Every XenAPI enum.\n\n");
    for (name, values) in enums {
        let ident = &names.enums[name];
        out.push_str("xapi_enum! {\n");
        let _ = writeln!(out, "/// `{name}`.");
        let _ = writeln!(out, "pub enum {ident} {{");
        let mut used = HashSet::new();
        for (value, doc) in values {
            let mut variant = camel(value);
            if variant == "Unknown" {
                variant = "UnknownValue".to_owned();
            }
            while !used.insert(variant.clone()) {
                variant.push('_');
            }
            write_doc(&mut out, doc);
            let _ = writeln!(out, "{variant} => {value:?},");
        }
        out.push_str("}\n}\n\n");
    }
    out
}

fn gen_records(classes: &[Class], names: &Names, sha: &str) -> Result<String, String> {
    let mut out = header(sha);
    out.push_str("//! One record struct per XenAPI class that has fields.\n\n");
    for c in classes {
        if live_fields(c).next().is_none() {
            continue;
        }
        let _ = writeln!(out, "/// Record of the `{}` class.", c.wire);
        out.push_str("///\n");
        write_doc(&mut out, &c.description);
        out.push_str(
            "#[derive(Debug, Clone, Default, PartialEq, ::serde::Serialize, ::serde::Deserialize)]\n\
             #[serde(default)]\n",
        );
        let _ = writeln!(out, "pub struct {} {{", record_ident(&c.wire));
        let mut used = HashSet::new();
        for f in live_fields(c) {
            let ident = snake(&f.wire);
            if !used.insert(ident.clone()) {
                return Err(format!("field clash {}.{}", c.wire, f.wire));
            }
            write_doc(&mut out, &f.description);
            if let Some(note) = lifecycle_note(&f.state) {
                out.push_str("///\n");
                let _ = writeln!(out, "/// {note}");
            }
            let mut attrs = Vec::new();
            if bare(&ident) != f.wire {
                attrs.push(format!("rename = {:?}", f.wire));
            }
            attrs.push(format!("deserialize_with = {:?}", deserialize_with(&f.ty)));
            let _ = writeln!(out, "#[serde({})]", attrs.join(", "));
            let _ = writeln!(out, "pub {ident}: {},", names.owned(&f.ty)?);
        }
        out.push_str("}\n\n");
    }
    Ok(out)
}

fn gen_classes(classes: &[Class], names: &Names, sha: &str) -> Result<String, String> {
    let mut out = header(sha);
    out.push_str("//! One marker type per XenAPI class.\n\n");
    let mut cached = Vec::new();
    for c in classes {
        let ident = names.class(&c.wire)?;
        let _ = writeln!(out, "/// Marker type for the `{}` class.", c.wire);
        out.push_str("///\n");
        write_doc(&mut out, &c.description);
        if let Some(note) = lifecycle_note(&c.state) {
            out.push_str("///\n");
            let _ = writeln!(out, "/// {note}");
        }
        let _ = writeln!(out, "#[derive(Debug)]\npub enum {ident} {{}}\n");
        let _ = writeln!(
            out,
            "impl crate::XapiClass for {ident} {{ const NAME: &'static str = {:?}; }}\n",
            c.wire
        );
        let has_record = live_fields(c).next().is_some();
        if has_record {
            let _ = writeln!(
                out,
                "impl crate::XapiObject for {ident} {{ type Record = crate::{}; }}\n",
                record_ident(&c.wire)
            );
        }
        let listable = live_messages(c).any(|m| m.wire == "get_all_records");
        if has_record && listable && !NOT_CACHED.contains(&c.wire.as_str()) {
            cached.push(format!(
                "({ident}, {}, {})",
                module_ident(&c.wire),
                plural(&c.wire)
            ));
        }
    }
    out.push_str(
        "/// Calls `$callback! { (Marker, module, plural), … }` with every class a\n\
         /// client-side cache keeps: classes that have a record and a\n\
         /// `get_all_records` call, except `session`, `user` and `secret`.\n\
         ///\n\
         /// `module` is the class's call module (`xapi::vm`) and `plural` a\n\
         /// snake-case name for a per-class map (`vms`).\n\
         #[macro_export]\n\
         macro_rules! for_each_cached_class {\n\
         ($callback:ident) => {\n\
         $callback! {\n",
    );
    for entry in &cached {
        let _ = writeln!(out, "{entry},");
    }
    out.push_str("}\n};\n}\n");
    Ok(out)
}

fn gen_calls(c: &Class, names: &Names, sha: &str) -> Result<String, String> {
    let mut out = header(sha);
    let _ = writeln!(out, "//! Calls on the `{}` class.\n", c.wire);
    let mut used = HashSet::new();
    for m in live_messages(c) {
        let fn_name = snake(&m.wire);
        let async_name = format!("async_{}", bare(&fn_name));
        if !used.insert(fn_name.clone()) || (m.is_async && !used.insert(async_name.clone())) {
            return Err(format!("message clash {}.{}", c.wire, m.wire));
        }
        let method = format!("{}.{}", c.wire, m.wire);

        // Signature pieces shared by the sync and Async variants.
        let mut args = vec![if m.has_session {
            "session: &crate::Session".to_owned()
        } else {
            "client: &crate::JsonRpcClient".to_owned()
        }];
        let mut required = Vec::new();
        let mut optional = Vec::new();
        let mut param_names = HashSet::new();
        for p in &m.params {
            let mut ident = snake(&p.wire);
            if matches!(bare(&ident), "session" | "client" | "params") {
                ident = format!("{}_", bare(&ident));
            }
            if !param_names.insert(ident.clone()) {
                return Err(format!("parameter clash in {method}"));
            }
            let ty = names.param(&p.ty)?;
            if p.required {
                args.push(format!("{ident}: {ty}"));
                let value = if by_value(&p.ty) {
                    format!("&{ident}")
                } else {
                    ident.clone()
                };
                required.push(format!("crate::params::arg({value})"));
            } else {
                args.push(format!("{ident}: Option<{ty}>"));
                optional.push(format!("{ident}.map(|v| crate::params::arg(&v))"));
            }
        }
        let receiver = if m.has_session { "session" } else { "client" };
        let build_params = if optional.is_empty() {
            format!("let params = vec![{}];", required.join(", "))
        } else {
            format!(
                "let mut params = vec![{}];\ncrate::params::push_optional(&mut params, {method:?}, vec![{}])?;",
                required.join(", "),
                optional.join(", ")
            )
        };

        write_doc(&mut out, &m.description);
        if let Some(note) = lifecycle_note(&m.state) {
            out.push_str("///\n");
            let _ = writeln!(out, "/// {note}");
        }
        if !m.errors.is_empty() {
            out.push_str("///\n");
            let errors: Vec<String> = m.errors.iter().map(|e| format!("`{e}`")).collect();
            let _ = writeln!(out, "/// Errors: {}.", errors.join(", "));
        }
        let ret = match &m.result {
            None => "()".to_owned(),
            Some(ty) => names.owned(ty)?,
        };
        let call = if m.result.is_none() {
            format!("{receiver}.call_void({method:?}, params).await")
        } else {
            format!("{receiver}.call({method:?}, params).await")
        };
        let _ = writeln!(
            out,
            "pub async fn {fn_name}({}) -> crate::Result<{ret}> {{\n{build_params}\n{call}\n}}\n",
            args.join(", ")
        );

        if m.is_async && m.has_session {
            let _ = writeln!(
                out,
                "/// `Async.{method}`: starts `{method}` as a server-side task and\n\
                 /// returns the task at once. Follow it through `task` events or\n\
                 /// `task.get_record`.\n\
                 pub async fn {async_name}({}) -> crate::Result<crate::Ref<crate::Task>> {{\n{build_params}\nsession.call(\"Async.{method}\", params).await\n}}\n",
                args.join(", ")
            );
        }
    }
    Ok(out)
}

/// Classes that `for_each_cached_class!` lists, for tests and tools.
pub fn cached_classes(xenapi: &str, async_msgs: &str) -> Result<BTreeSet<String>, String> {
    let (classes, _) = parse_model(xenapi, async_msgs)?;
    Ok(classes
        .iter()
        .filter(|c| {
            live_fields(c).next().is_some()
                && live_messages(c).any(|m| m.wire == "get_all_records")
                && !NOT_CACHED.contains(&c.wire.as_str())
        })
        .map(|c| c.wire.clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_types() {
        assert_eq!(
            parse_ty("(VM ref -> (string -> string) map) map").unwrap(),
            Ty::Map(
                Box::new(Ty::Ref("VM".into())),
                Box::new(Ty::Map(Box::new(Ty::Str), Box::new(Ty::Str)))
            )
        );
        assert_eq!(
            parse_ty("enum vm_operations set").unwrap(),
            Ty::Set(Box::new(Ty::Enum("vm_operations".into())))
        );
        assert!(parse_ty("mystery").is_err());
    }

    #[test]
    fn names() {
        assert_eq!(camel("VM_guest_metrics"), "VmGuestMetrics");
        assert_eq!(camel("changing_VCPUs"), "ChangingVcpus");
        assert_eq!(camel("active-backup"), "ActiveBackup");
        assert_eq!(snake("resident_VMs"), "resident_vms");
        assert_eq!(snake("type"), "r#type");
        assert_eq!(snake("self"), "self_");
        assert_eq!(plural("VM"), "vms");
        assert_eq!(plural("VM_guest_metrics"), "vm_guest_metrics");
        assert_eq!(plural("Repository"), "repositories");
        assert_eq!(plural("pool_patch"), "pool_patches");
        assert_eq!(class_ident("session"), "SessionClass");
    }

    #[test]
    fn escapes_docs() {
        assert_eq!(escape_doc("  a [b] <c> `d`  "), "a \\[b\\] \\<c\\> 'd'");
        assert_eq!(escape_doc("# x #y"), "\\# x #y");
    }
}
