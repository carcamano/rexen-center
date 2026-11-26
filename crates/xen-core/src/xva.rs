//! XVA files (XAPI's VM export format) on the client side.
//!
//! An XVA is a tar archive: `ova.xml` (the VM's metadata, XML-RPC style)
//! first, then each disk as numbered blocks (`Ref:12/00000000`, usually
//! 1 MiB), each followed by its checksum file (`….checksum`, SHA-1 in
//! lower-case hex; or `….xxhash`, XXH64 in upper-case hex), and the two
//! zero blocks that end a tar archive. Older servers wrote one
//! `checksum.xml` table instead.
//!
//! [`Verifier`] checks a stream as it passes, piece by piece, like the
//! original's `CommandLib.Export.verify`: every tar header's checksum,
//! every block's checksum, and that the archive ends properly. A server
//! sends an export without a length and ends it by closing the
//! connection, so a stream cut short is only noticed this way. Nothing is
//! held but the current header and the running hashes.
//!
//! Also here: reading `ova.xml` from a file and summing its disks' sizes
//! (`ImportSourcePage.GetDiskCapacityXva`), uncompressing an `.xva.gz`
//! (`ImportSourcePage.Uncompress`) and the free space of a folder
//! (`ExportSelectVMsPage.GetFreeSpace`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use xapi::{Host, Network, NetworkRecord, Ref, Sr, SrRecord, VbdType, Vm, VmPowerState, VmRecord};

use crate::model::ConnectionModel;
use crate::objects::{self, VmKind};

/// Tar block size.
const BLOCK: usize = 512;
/// The largest checksum file accepted (a hex digest is 16 or 40 bytes).
const MAX_CHECKSUM_FILE: u64 = 1024;
/// The largest `checksum.xml` or `ova.xml` read into memory.
const MAX_METADATA: u64 = 64 << 20;

/// Why an XVA stream is not valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XvaError {
    /// A tar header's checksum is wrong (`HeaderChecksumFailed`).
    HeaderChecksum,
    /// A block's checksum is wrong (`BlockChecksumFailed`).
    BlockChecksum { block: String },
    /// The stream ended before the end of the archive.
    Truncated,
    /// Not an archive the server writes (`FormatException`).
    Format(String),
}

impl std::fmt::Display for XvaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HeaderChecksum => f.write_str("a tar header checksum is wrong"),
            Self::BlockChecksum { block } => write!(f, "the checksum of block {block} is wrong"),
            Self::Truncated => f.write_str("the archive ends early"),
            Self::Format(why) => write!(f, "not an XVA archive: {why}"),
        }
    }
}

impl std::error::Error for XvaError {}

// ---- XXH64 -----------------------------------------------------------------------------

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

fn merge(acc: u64, value: u64) -> u64 {
    (acc ^ round(0, value)).wrapping_mul(P1).wrapping_add(P4)
}

/// XXH64 with seed 0, fed piece by piece (`YYProject.XXHash.XXHash64`).
#[derive(Clone)]
pub struct Xxh64 {
    acc: [u64; 4],
    buf: [u8; 32],
    buffered: usize,
    total: u64,
}

impl Default for Xxh64 {
    fn default() -> Self {
        Self {
            acc: [P1.wrapping_add(P2), P2, 0, 0u64.wrapping_sub(P1)],
            buf: [0; 32],
            buffered: 0,
            total: 0,
        }
    }
}

impl Xxh64 {
    fn stripe(acc: &mut [u64; 4], s: &[u8]) {
        for (i, a) in acc.iter_mut().enumerate() {
            let lane = u64::from_le_bytes(s[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
            *a = round(*a, lane);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.buffered > 0 {
            let take = (32 - self.buffered).min(data.len());
            self.buf[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered < 32 {
                return;
            }
            let buf = self.buf;
            Self::stripe(&mut self.acc, &buf);
            self.buffered = 0;
        }
        while data.len() >= 32 {
            Self::stripe(&mut self.acc, &data[..32]);
            data = &data[32..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.buffered = data.len();
    }

    pub fn digest(&self) -> u64 {
        let [a1, a2, a3, a4] = self.acc;
        let mut h = if self.total >= 32 {
            let mut h = a1
                .rotate_left(1)
                .wrapping_add(a2.rotate_left(7))
                .wrapping_add(a3.rotate_left(12))
                .wrapping_add(a4.rotate_left(18));
            for a in self.acc {
                h = merge(h, a);
            }
            h
        } else {
            P5
        };
        h = h.wrapping_add(self.total);
        let mut rest = &self.buf[..self.buffered];
        while rest.len() >= 8 {
            let k = round(
                0,
                u64::from_le_bytes(rest[..8].try_into().expect("8 bytes")),
            );
            h = (h ^ k).rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
            rest = &rest[8..];
        }
        if rest.len() >= 4 {
            let k = u64::from(u32::from_le_bytes(rest[..4].try_into().expect("4 bytes")));
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
        h ^ (h >> 32)
    }

    /// The digest as the original writes it: big-endian, upper-case hex.
    pub fn hex(&self) -> String {
        format!("{:016X}", self.digest())
    }
}

// ---- the tar stream --------------------------------------------------------------------

/// Which checksum a block file carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockChecksum {
    /// `….checksum`: SHA-1, lower-case hex.
    Sha1,
    /// `….xxhash`: XXH64, upper-case hex.
    XxHash,
}

/// One tar header.
#[derive(Debug, Clone)]
struct Header {
    name: String,
    size: u64,
}

fn octal(field: &[u8]) -> Result<u64, XvaError> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        // Base-256 (GNU) for large sizes.
        let mut v = u64::from(field[0] & 0x7f);
        for b in &field[1..] {
            v = v
                .checked_mul(256)
                .and_then(|v| v.checked_add(u64::from(*b)))
                .ok_or_else(|| XvaError::Format("size out of range".into()))?;
        }
        return Ok(v);
    }
    let text: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| XvaError::Format(format!("bad octal field {text:?}")))
}

fn parse_header(b: &[u8; BLOCK]) -> Result<Header, XvaError> {
    let stored = octal(&b[148..156])?;
    let computed: u64 = b
        .iter()
        .enumerate()
        .map(|(i, x)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(*x)
            }
        })
        .sum();
    if stored != computed {
        return Err(XvaError::HeaderChecksum);
    }
    let name_end = b[..100].iter().position(|c| *c == 0).unwrap_or(100);
    let mut name = String::from_utf8_lossy(&b[..name_end])
        .trim_end_matches([' ', '\0'])
        .to_owned();
    // ustar prefix, should a long name have been split.
    if &b[257..262] == b"ustar" && b[345] != 0 {
        let end = b[345..500].iter().position(|c| *c == 0).unwrap_or(155);
        let prefix = String::from_utf8_lossy(&b[345..345 + end]).to_string();
        name = format!("{prefix}/{name}");
    }
    Ok(Header {
        name,
        size: octal(&b[124..136])?,
    })
}

fn padding(size: u64) -> u64 {
    (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64
}

/// Hashes of the block being read: both until the archive says which it
/// uses.
#[derive(Clone)]
struct BlockHash {
    sha1: Option<ring::digest::Context>,
    xxh: Option<Xxh64>,
}

impl BlockHash {
    fn new(kind: Option<BlockChecksum>) -> Self {
        Self {
            sha1: (kind != Some(BlockChecksum::XxHash))
                .then(|| ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY)),
            xxh: (kind != Some(BlockChecksum::Sha1)).then(Xxh64::default),
        }
    }

    fn update(&mut self, data: &[u8]) {
        if let Some(s) = &mut self.sha1 {
            s.update(data);
        }
        if let Some(x) = &mut self.xxh {
            x.update(data);
        }
    }

    fn hex(&self, kind: BlockChecksum) -> Option<String> {
        match kind {
            BlockChecksum::Sha1 => self.sha1.clone().map(|s| {
                s.finish()
                    .as_ref()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect()
            }),
            BlockChecksum::XxHash => self.xxh.as_ref().map(Xxh64::hex),
        }
    }
}

enum Entry {
    /// `ova.xml`, kept when asked (up to [`MAX_METADATA`]).
    Metadata(Option<Vec<u8>>),
    /// A legacy `checksum.xml` table.
    Table(Vec<u8>),
    /// A disk block.
    Block { name: String, hash: Box<BlockHash> },
    /// The checksum file of `block`.
    Checksum {
        block: String,
        kind: BlockChecksum,
        text: Vec<u8>,
    },
}

enum Stage {
    /// Reading a header (`zeroes`: one zero block seen).
    Header { zeroes: bool },
    /// Reading an entry's content, then its padding.
    Content {
        entry: Entry,
        remaining: u64,
        pad: u64,
    },
    /// Skipping padding.
    Padding { remaining: u64 },
    /// The end-of-archive marker was read.
    Ended,
}

/// What a verified archive held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XvaSummary {
    /// Bytes of the archive up to its end marker.
    pub bytes: u64,
    /// Disk blocks checked.
    pub blocks: u64,
    /// The checksum the blocks carry (`None`: no block, or a table).
    pub checksum: Option<BlockChecksum>,
    /// `ova.xml`, when [`Verifier::keep_metadata`] asked for it.
    pub metadata: Option<String>,
}

/// Checks an XVA stream piece by piece (see the module documentation).
pub struct Verifier {
    stage: Stage,
    header: Vec<u8>,
    kind: Option<BlockChecksum>,
    /// A block read, waiting for its checksum file: name and hashes.
    pending: Option<(String, Box<BlockHash>)>,
    /// Blocks checked against a legacy table: name → recomputed SHA-1.
    recomputed: Vec<(String, String)>,
    table: Option<Vec<(String, String)>>,
    keep_metadata: bool,
    summary: XvaSummary,
}

impl Default for Verifier {
    fn default() -> Self {
        Self {
            stage: Stage::Header { zeroes: false },
            header: Vec::with_capacity(BLOCK),
            kind: None,
            pending: None,
            recomputed: Vec::new(),
            table: None,
            keep_metadata: false,
            summary: XvaSummary::default(),
        }
    }
}

impl Verifier {
    /// Keeps `ova.xml` for [`XvaSummary::metadata`].
    pub fn keep_metadata(mut self) -> Self {
        self.keep_metadata = true;
        self
    }

    /// Whether the end-of-archive marker has been read.
    pub fn ended(&self) -> bool {
        matches!(self.stage, Stage::Ended)
    }

    /// Checks the next piece of the stream.
    pub fn feed(&mut self, mut data: &[u8]) -> Result<(), XvaError> {
        while !data.is_empty() {
            match &mut self.stage {
                // Whatever follows the marker (tar record padding) is ignored.
                Stage::Ended => return Ok(()),
                Stage::Header { .. } => {
                    let take = (BLOCK - self.header.len()).min(data.len());
                    self.header.extend_from_slice(&data[..take]);
                    self.summary.bytes += take as u64;
                    data = &data[take..];
                    if self.header.len() == BLOCK {
                        self.on_header()?;
                    }
                }
                Stage::Content {
                    entry,
                    remaining,
                    pad,
                } => {
                    let take = usize::try_from(*remaining)
                        .unwrap_or(usize::MAX)
                        .min(data.len());
                    let piece = &data[..take];
                    match entry {
                        Entry::Metadata(Some(buf)) | Entry::Table(buf) => {
                            buf.extend_from_slice(piece)
                        }
                        Entry::Checksum { text, .. } => text.extend_from_slice(piece),
                        Entry::Metadata(None) => {}
                        Entry::Block { hash, .. } => hash.update(piece),
                    }
                    *remaining -= take as u64;
                    self.summary.bytes += take as u64;
                    data = &data[take..];
                    if *remaining == 0 {
                        let pad = *pad;
                        let Stage::Content { entry, .. } =
                            std::mem::replace(&mut self.stage, Stage::Padding { remaining: pad })
                        else {
                            unreachable!("content stage")
                        };
                        self.on_entry(entry)?;
                        if pad == 0 {
                            self.stage = Stage::Header { zeroes: false };
                        }
                    }
                }
                Stage::Padding { remaining } => {
                    let take = usize::try_from(*remaining)
                        .unwrap_or(usize::MAX)
                        .min(data.len());
                    *remaining -= take as u64;
                    self.summary.bytes += take as u64;
                    data = &data[take..];
                    if *remaining == 0 {
                        self.stage = Stage::Header { zeroes: false };
                    }
                }
            }
        }
        Ok(())
    }

    fn on_header(&mut self) -> Result<(), XvaError> {
        let block: [u8; BLOCK] = std::mem::take(&mut self.header)
            .try_into()
            .expect("a whole block");
        self.header = Vec::with_capacity(BLOCK);
        let Stage::Header { zeroes } = self.stage else {
            unreachable!("header stage")
        };
        if block.iter().all(|b| *b == 0) {
            if let Some((name, _)) = &self.pending {
                return Err(XvaError::Format(format!("block {name} has no checksum")));
            }
            self.stage = if zeroes {
                Stage::Ended
            } else {
                Stage::Header { zeroes: true }
            };
            return Ok(());
        }
        let header = parse_header(&block)?;
        let checksum_kind = if header.name.ends_with(".xxhash") {
            Some(BlockChecksum::XxHash)
        } else if header.name.ends_with(".checksum") {
            Some(BlockChecksum::Sha1)
        } else {
            None
        };
        let entry = match (&self.pending, checksum_kind) {
            (Some((block, _)), Some(kind)) => {
                let expected = header
                    .name
                    .strip_suffix(if kind == BlockChecksum::Sha1 {
                        ".checksum"
                    } else {
                        ".xxhash"
                    })
                    .unwrap_or_default();
                if expected != block {
                    return Err(XvaError::Format(format!(
                        "{} does not follow its block",
                        header.name
                    )));
                }
                if header.size > MAX_CHECKSUM_FILE {
                    return Err(XvaError::Format(format!("{} is too large", header.name)));
                }
                Entry::Checksum {
                    block: block.clone(),
                    kind,
                    text: Vec::new(),
                }
            }
            (Some((block, _)), None) => {
                return Err(XvaError::Format(format!("block {block} has no checksum")));
            }
            (None, Some(_)) => {
                return Err(XvaError::Format(format!("{} has no block", header.name)));
            }
            (None, None) if header.name == "ova.xml" => {
                Entry::Metadata((self.keep_metadata && header.size <= MAX_METADATA).then(Vec::new))
            }
            (None, None) if header.name.ends_with("checksum.xml") => {
                if header.size > MAX_METADATA {
                    return Err(XvaError::Format("checksum.xml is too large".into()));
                }
                Entry::Table(Vec::new())
            }
            (None, None) => Entry::Block {
                name: header.name.clone(),
                hash: Box::new(BlockHash::new(self.kind)),
            },
        };
        self.stage = Stage::Content {
            entry,
            remaining: header.size,
            pad: padding(header.size),
        };
        if header.size == 0 {
            // An empty entry ends at once.
            let Stage::Content { entry, .. } =
                std::mem::replace(&mut self.stage, Stage::Header { zeroes: false })
            else {
                unreachable!("content stage")
            };
            self.on_entry(entry)?;
        }
        Ok(())
    }

    fn on_entry(&mut self, entry: Entry) -> Result<(), XvaError> {
        match entry {
            Entry::Metadata(kept) => {
                if let Some(bytes) = kept {
                    self.summary.metadata = Some(String::from_utf8_lossy(&bytes).into_owned());
                }
            }
            Entry::Table(bytes) => {
                self.table = Some(parse_checksum_table(&String::from_utf8_lossy(&bytes)));
            }
            Entry::Block { name, hash } => {
                if self.table.is_some() {
                    // Legacy archives: blocks are listed in checksum.xml.
                    let sha1 = hash.hex(BlockChecksum::Sha1).unwrap_or_default();
                    self.recomputed.push((name, sha1));
                    self.summary.blocks += 1;
                } else {
                    self.pending = Some((name, hash));
                }
            }
            Entry::Checksum { block, kind, text } => {
                let (_, hash) = self.pending.take().expect("a pending block");
                let ours = hash.hex(kind).unwrap_or_default();
                let theirs = String::from_utf8_lossy(&text).trim().to_owned();
                if !ours.eq_ignore_ascii_case(&theirs) {
                    return Err(XvaError::BlockChecksum { block });
                }
                self.kind = Some(kind);
                self.summary.checksum = Some(kind);
                self.summary.blocks += 1;
            }
        }
        Ok(())
    }

    /// The stream ended: checks that the archive did too.
    pub fn finish(self) -> Result<XvaSummary, XvaError> {
        if !self.ended() {
            return Err(XvaError::Truncated);
        }
        if let Some(table) = &self.table {
            for (name, ours) in &self.recomputed {
                let theirs = table.iter().find(|(n, _)| n == name).map(|(_, v)| v);
                if theirs.is_none_or(|t| !t.eq_ignore_ascii_case(ours)) {
                    return Err(XvaError::BlockChecksum {
                        block: name.clone(),
                    });
                }
            }
        }
        Ok(self.summary)
    }
}

/// `Export.parse_checksum_table`: `<member><name>…</name><value>…</value>`.
fn parse_checksum_table(xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for member in xml.split("<member>").skip(1) {
        let name = between(member, "<name>", "</name>").map(strip_tags);
        let value = between(member, "<value>", "</value>").map(strip_tags);
        if let (Some(n), Some(v)) = (name, value) {
            out.push((n.trim().to_owned(), v.trim().to_owned()));
        }
    }
    out
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let to = text[from..].find(end)? + from;
    Some(&text[from..to])
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

// ---- files ----------------------------------------------------------------------------

/// The first entry of the XVA at `path`, read as text
/// (`ImportSourcePage.GetXmlStringFromTarXVA`), if it is `ova.xml`.
pub fn read_metadata(path: &Path) -> std::io::Result<Option<String>> {
    let mut file = std::fs::File::open(path)?;
    let mut block = [0u8; BLOCK];
    file.read_exact(&mut block)?;
    let header = parse_header(&block).map_err(std::io::Error::other)?;
    if header.name != "ova.xml" || header.size > MAX_METADATA {
        return Ok(None);
    }
    let mut text = Vec::new();
    file.take(header.size).read_to_end(&mut text)?;
    Ok(Some(String::from_utf8_lossy(&text).into_owned()))
}

/// `ImportSourcePage.GetTotalSizeFromXmlXva`: the sum of every
/// `virtual_size` in the metadata (`None` if one cannot be read).
pub fn total_virtual_size(xml: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut rest = xml;
    while let Some(i) = rest.find("<name>virtual_size</name>") {
        rest = &rest[i + "<name>virtual_size</name>".len()..];
        let value = between(rest, "<value>", "</value>")?;
        total = total.checked_add(strip_tags(value).trim().parse().ok()?)?;
    }
    Some(total)
}

/// The size of the disks in the XVA at `path`, as the import wizard shows
/// it: the sum of the disks' sizes, or the file's size when the metadata
/// cannot be read (`GetDiskCapacityXva`).
pub fn disk_capacity(path: &Path) -> u64 {
    read_metadata(path)
        .ok()
        .flatten()
        .and_then(|xml| total_virtual_size(&xml))
        .unwrap_or_else(|| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
}

/// What `name.xva.gz` uncompresses to: `name.xva` in the same folder.
pub fn uncompressed_path(path: &Path) -> PathBuf {
    path.with_extension("")
}

/// Uncompresses the gzip file `from` into `to` (`ImportSourcePage`'s
/// unzip worker). `progress` is told the compressed bytes read so far;
/// `cancelled` is asked between pieces. A cancelled or failed run removes
/// what it wrote.
pub fn uncompress(
    from: &Path,
    to: &Path,
    progress: &dyn Fn(u64),
    cancelled: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    struct Counting<R> {
        inner: R,
        read: u64,
    }
    impl<R: Read> Read for Counting<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.read += n as u64;
            Ok(n)
        }
    }
    let result = (|| {
        let input = Counting {
            inner: std::io::BufReader::new(std::fs::File::open(from)?),
            read: 0,
        };
        let mut gz = flate2::read::GzDecoder::new(input);
        let mut out = std::io::BufWriter::new(std::fs::File::create(to)?);
        let mut buf = vec![0u8; 64 << 10];
        loop {
            if cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "cancelled",
                ));
            }
            let n = gz.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            progress(gz.get_ref().read);
        }
        out.flush()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(to);
    }
    result
}

/// Free bytes for this user in the file system holding `dir`, when known.
#[cfg(unix)]
pub fn free_space(dir: &Path) -> Option<u64> {
    let s = rustix::fs::statvfs(dir).ok()?;
    Some(s.f_bavail.saturating_mul(s.f_frsize))
}

/// Free bytes for this user in the file system holding `dir`, when known.
#[cfg(not(unix))]
pub fn free_space(_dir: &Path) -> Option<u64> {
    None
}

// ---- the wizards' rules -----------------------------------------------------------------

/// The user's Downloads folder (`KnownFolders.Downloads`), the export
/// wizard's default destination.
pub fn downloads_folder() -> Option<PathBuf> {
    let dirs = directories::UserDirs::new()?;
    dirs.download_dir()
        .map(Path::to_path_buf)
        .or_else(|| Some(dirs.home_dir().to_path_buf()))
}

/// `PathValidator.IsFileNameValid`: not empty, no path separators or
/// characters Windows refuses, not `.` or `..`.
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

/// `ExportCommand.CanExportVm`: a VM (not a template) the server lets
/// export, with no operation of this console running on it.
pub fn can_export_vm(vm: &VmRecord, busy: bool) -> bool {
    !vm.is_a_template && !busy && vm.allowed_operations.contains(&xapi::VmOperations::Export)
}

/// `ExportTemplateCommand` (a template, not a snapshot) and
/// `ExportSnapshotAsTemplateCommand` (a snapshot): Export to File.
pub fn can_export_to_file(vm: &VmRecord, busy: bool) -> bool {
    match objects::vm_kind(vm) {
        VmKind::Snapshot => true,
        VmKind::UserTemplate | VmKind::DefaultTemplate => {
            !busy && vm.allowed_operations.contains(&xapi::VmOperations::Export)
        }
        _ => false,
    }
}

/// `ExportSelectVMsPage.IsVmExportable`: shown guests that are halted or
/// suspended and may be exported.
pub fn exportable_vms<'a>(
    model: &'a ConnectionModel,
    show_hidden: bool,
    busy: &std::collections::HashSet<Ref<Vm>>,
) -> Vec<(&'a Ref<Vm>, &'a Arc<VmRecord>)> {
    let mut v: Vec<_> = model
        .vms
        .iter()
        .filter(|(_, vm)| objects::vm_kind(vm) == VmKind::Guest)
        .filter(|(_, vm)| objects::vm_is_shown(vm, show_hidden))
        .filter(|(_, vm)| {
            matches!(
                vm.power_state,
                VmPowerState::Halted | VmPowerState::Suspended
            )
        })
        .filter(|(r, vm)| can_export_vm(vm, busy.contains(*r)))
        .collect();
    v.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label));
    v
}

/// `ExportSelectVMsPage.GetTotalVmSize` / `VM.GetRecommendedExportSpace`:
/// the virtual size of the VM's disks (DVD drives and the tools ISO left
/// out).
pub fn export_space(model: &ConnectionModel, vm: &VmRecord) -> i64 {
    vm.vbds
        .iter()
        .filter_map(|b| model.vbds.get(b))
        .filter(|b| b.r#type != VbdType::Cd)
        .filter_map(|b| model.vdis.get(&b.vdi))
        .filter(|d| {
            model
                .srs
                .get(&d.sr)
                .is_none_or(|sr| !objects::is_tools_sr(sr))
        })
        .map(|d| d.virtual_size)
        .sum()
}

/// The SRs an import can put its disks on (`SrPicker` for a VM): not ISO
/// or tools SRs, attached and working, seen by `host` when one is chosen.
/// The pool's default SR comes first.
pub fn import_srs<'a>(
    model: &'a ConnectionModel,
    host: Option<&Ref<Host>>,
) -> Vec<(&'a Ref<Sr>, &'a Arc<SrRecord>)> {
    let default = model.pool().map(|(_, p)| p.default_sr.clone());
    let mut v: Vec<_> = model
        .srs
        .iter()
        .filter(|(_, sr)| sr.content_type != "iso" && !objects::is_tools_sr(sr))
        .filter(|(_, sr)| !objects::sr_is_broken(model, sr) && !objects::sr_is_detached(model, sr))
        .filter(|(_, sr)| sr.physical_size > 0)
        .filter(|(_, sr)| match host {
            Some(h) => {
                sr.shared
                    || sr
                        .pbds
                        .iter()
                        .filter_map(|p| model.pbds.get(p))
                        .any(|p| &p.host == h)
            }
            None => true,
        })
        .collect();
    v.sort_by(|a, b| {
        (Some(b.0) == default.as_ref())
            .cmp(&(Some(a.0) == default.as_ref()))
            .then_with(|| objects::natural_cmp(&a.1.name_label, &b.1.name_label))
    });
    v
}

/// `NetworkPickerPage.ShowNetwork`: shown, not a bond member, not SR-IOV,
/// and seen by the home server (or by every server when there is none).
pub fn import_networks<'a>(
    model: &'a ConnectionModel,
    affinity: Option<&Ref<Host>>,
    show_hidden: bool,
) -> Vec<(&'a Ref<Network>, &'a Arc<NetworkRecord>)> {
    let sees = |net: &NetworkRecord, host: &Ref<Host>| {
        net.pifs.is_empty()
            || net
                .pifs
                .iter()
                .filter_map(|p| model.pifs.get(p))
                .any(|p| &p.host == host)
    };
    objects::shown_networks(model, show_hidden)
        .into_iter()
        .filter(|(_, n)| !objects::is_bond_member_network(model, n))
        .filter(|(_, n)| !crate::network::network_is_sriov(model, n))
        .filter(|(_, n)| match affinity {
            Some(h) => sees(n, h),
            None => model.hosts.keys().all(|h| sees(n, h)),
        })
        .collect()
}

/// `VM.MaxVIFsAllowed`: the template's "number-of-vifs", else 7.
pub fn max_vifs(vm: &VmRecord) -> usize {
    crate::provisioning::recommendation(vm, "number-of-vifs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(7)
}

/// One row of the import wizard's network page: device, MAC (empty:
/// generated), network.
pub type ImportVifRow = (String, String, Ref<Network>);

/// `NetworkPickerPage.BuildVIFList`: a VM's own interfaces; a template's
/// with new MACs; a template without any gets one per network that plugs
/// automatically. An interface on a network that is not offered moves to
/// the first offered one (the guest installer network excepted).
pub fn default_import_vifs(
    model: &ConnectionModel,
    vm: &VmRecord,
    offered: &[(&Ref<Network>, &Arc<NetworkRecord>)],
) -> Vec<ImportVifRow> {
    let first = offered.first().map(|(r, _)| (*r).clone());
    let fix = |net: &Ref<Network>| {
        let ok = offered.iter().any(|(r, _)| *r == net)
            || model
                .networks
                .get(net)
                .is_some_and(|n| objects::is_guest_installer_network(n));
        if ok { Some(net.clone()) } else { first.clone() }
    };
    let mut vifs: Vec<_> = vm.vifs.iter().filter_map(|r| model.vifs.get(r)).collect();
    vifs.sort_by(|a, b| objects::natural_cmp(&a.device, &b.device));
    if vm.is_a_template && vifs.is_empty() {
        return offered
            .iter()
            .filter(|(_, n)| crate::network::network_auto_plug(n))
            .take(max_vifs(vm))
            .enumerate()
            .map(|(i, (r, _))| (i.to_string(), String::new(), (*r).clone()))
            .collect();
    }
    vifs.into_iter()
        .filter_map(|v| {
            let mac = if vm.is_a_template {
                String::new()
            } else {
                v.mac.clone()
            };
            Some((v.device.clone(), mac, fix(&v.network)?))
        })
        .collect()
}

/// A tar archive writer for tests and tools: entries and the end marker.
pub mod tar {
    fn header(name: &str, size: u64) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len().min(100)].copy_from_slice(&name.as_bytes()[..name.len().min(100)]);
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[156] = b'0';
        h[148..156].copy_from_slice(b"        ");
        let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    /// One entry: header, content, padding.
    pub fn entry(name: &str, content: &[u8]) -> Vec<u8> {
        let mut out = header(name, content.len() as u64).to_vec();
        out.extend_from_slice(content);
        out.resize(out.len() + super::padding(content.len() as u64) as usize, 0);
        out
    }

    /// The end-of-archive marker.
    pub fn end() -> Vec<u8> {
        vec![0u8; 1024]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xxh64_matches_the_reference() {
        let hash = |data: &[u8]| {
            let mut h = Xxh64::default();
            h.update(data);
            h.digest()
        };
        assert_eq!(hash(b""), 0xEF46_DB37_51D8_E999);
        assert_eq!(hash(b"a"), 0xD24E_C4F1_A98C_6E5B);
        assert_eq!(hash(b"abc"), 0x44BC_2CF5_AD77_0999);
        let long: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        // Fed in uneven pieces, the digest is the same.
        let mut pieces = Xxh64::default();
        for c in long.chunks(13) {
            pieces.update(c);
        }
        assert_eq!(pieces.digest(), hash(&long));
        assert_eq!(
            hash(b"Nobody inspects the spammish repetition"),
            0xFBCE_A83C_8A37_8BF1
        );
    }

    fn sha1_hex(data: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, data)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn xxh_hex(data: &[u8]) -> String {
        let mut h = Xxh64::default();
        h.update(data);
        h.hex()
    }

    fn archive(xxhash: bool, corrupt: bool) -> Vec<u8> {
        let mut out = tar::entry(
            "ova.xml",
            b"<value><struct><member><name>virtual_size</name><value>4096</value></member></struct></value>",
        );
        for i in 0..3u8 {
            let block = vec![i + 1; 2048];
            let name = format!("Ref:9/{i:08}");
            out.extend(tar::entry(&name, &block));
            let (ext, mut sum) = if xxhash {
                ("xxhash", xxh_hex(&block))
            } else {
                ("checksum", sha1_hex(&block))
            };
            if corrupt && i == 1 {
                sum = sum.replace(&sum[..1], if &sum[..1] == "0" { "1" } else { "0" });
            }
            out.extend(tar::entry(&format!("{name}.{ext}"), sum.as_bytes()));
        }
        out.extend(tar::end());
        out
    }

    #[test]
    fn a_whole_archive_verifies_in_any_pieces() {
        for xxhash in [false, true] {
            let data = archive(xxhash, false);
            for piece in [1, 7, 512, 4096, data.len()] {
                let mut v = Verifier::default().keep_metadata();
                for c in data.chunks(piece) {
                    v.feed(c).unwrap();
                }
                let s = v.finish().unwrap();
                assert_eq!(s.blocks, 3);
                assert_eq!(s.bytes, data.len() as u64);
                assert_eq!(
                    s.checksum,
                    Some(if xxhash {
                        BlockChecksum::XxHash
                    } else {
                        BlockChecksum::Sha1
                    })
                );
                assert_eq!(total_virtual_size(&s.metadata.unwrap()), Some(4096));
            }
        }
    }

    #[test]
    fn broken_archives_are_refused() {
        let data = archive(true, true);
        let mut v = Verifier::default();
        assert_eq!(
            v.feed(&data),
            Err(XvaError::BlockChecksum {
                block: "Ref:9/00000001".into()
            })
        );
        // Cut short anywhere, including just before the end marker.
        let data = archive(false, false);
        for cut in [100, 3000, data.len() - 1024, data.len() - 1] {
            let mut v = Verifier::default();
            v.feed(&data[..cut]).unwrap();
            assert_eq!(v.finish().unwrap_err(), XvaError::Truncated);
        }
        // A damaged header.
        let mut data = archive(false, false);
        data[10] ^= 0x20;
        assert_eq!(
            Verifier::default().feed(&data),
            Err(XvaError::HeaderChecksum)
        );
        // A block without its checksum.
        let mut data = tar::entry("ova.xml", b"<x/>");
        data.extend(tar::entry("Ref:1/00000000", &[1; 512]));
        data.extend(tar::end());
        assert!(matches!(
            Verifier::default().feed(&data),
            Err(XvaError::Format(_))
        ));
    }

    #[test]
    fn legacy_checksum_tables() {
        let block = vec![5u8; 1024];
        let mut data = tar::entry("ova.xml", b"<x/>");
        let table = format!(
            "<value><struct><member><name>Ref:3/00000000</name><value>{}</value></member></struct></value>",
            sha1_hex(&block)
        );
        data.extend(tar::entry("checksum.xml", table.as_bytes()));
        data.extend(tar::entry("Ref:3/00000000", &block));
        data.extend(tar::end());
        let mut v = Verifier::default();
        v.feed(&data).unwrap();
        assert_eq!(v.finish().unwrap().blocks, 1);
    }

    #[test]
    fn metadata_and_capacity() {
        let dir = std::env::temp_dir().join(format!("xva-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.xva");
        std::fs::write(&path, archive(true, false)).unwrap();
        assert_eq!(disk_capacity(&path), 4096);
        std::fs::write(&path, b"not a tar file at all").unwrap();
        assert_eq!(disk_capacity(&path), 21);
        let xml = "<member><name>virtual_size</name><value><string>10</string></value></member>\
                   <member><name>virtual_size</name><value>32</value></member>";
        assert_eq!(total_virtual_size(xml), Some(42));
        assert_eq!(
            total_virtual_size("<name>virtual_size</name><value>x</value>"),
            None
        );
        // gzip round trip.
        let gz = dir.join("b.xva.gz");
        {
            let f = std::fs::File::create(&gz).unwrap();
            let mut e = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            e.write_all(&archive(false, false)).unwrap();
            e.finish().unwrap();
        }
        let out = uncompressed_path(&gz);
        assert_eq!(out, dir.join("b.xva"));
        uncompress(&gz, &out, &|_| {}, &|| false).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), archive(false, false));
        assert!(uncompress(&gz, &out, &|_| {}, &|| true).is_err());
        assert!(!out.exists());
        assert!(free_space(&dir).is_none_or(|f| f > 0));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
