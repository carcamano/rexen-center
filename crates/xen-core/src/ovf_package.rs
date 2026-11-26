//! OVF packages (F11b): a folder holding `<name>.ovf` and its files, or an
//! OVA archive; their manifest and signature, as the original's
//! `XenOvfApi.Package`.
//!
//! * [`Package::open`] reads the descriptor of a `.ovf` (its files beside
//!   it) or of an `.ova` (the first `*.ovf` entry; the archive's entries are
//!   listed from their headers only). `<name>.mf` and `<name>.cert` are the
//!   manifest and the signature when their base name is the descriptor's.
//! * [`Package::verify_manifest`] hashes the files the manifest lists
//!   (SHA-1, SHA-256, SHA-384 or SHA-512), streaming; in an OVA every entry
//!   but the manifest and the certificate must be listed, and every listed
//!   file must exist.
//! * [`Package::verify_signature`] checks the `.cert`'s RSA signature of the
//!   manifest with the certificate's public key: the standard form (the
//!   signature of the manifest's bytes) and the original console's own
//!   (for SHA-256 it signs the manifest's hash, so the signed data is
//!   hashed twice). The certificate may be PEM of DER, or of the Windows
//!   serialised form the original writes. Whether the certificate is
//!   trusted (its chain, its dates, revocation) is **not** checked:
//!   [`Signature::trust_checked`] is always false, and the wizard says so.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::ova::{self, OvaEntry};
use crate::ovf::Envelope;

/// Where the package's files are.
#[derive(Debug, Clone)]
pub enum Layout {
    /// A folder: each file at `dir/href` (or `href` when absolute).
    Folder(PathBuf),
    /// An OVA: the archive and its entries.
    Ova(PathBuf, Vec<OvaEntry>),
}

/// An opened package.
#[derive(Debug, Clone)]
pub struct Package {
    /// The `.ovf` or `.ova` chosen.
    pub path: PathBuf,
    /// The descriptor's file name and base name.
    pub descriptor: String,
    pub name: String,
    pub layout: Layout,
    pub envelope: Envelope,
    /// The descriptor's bytes (its manifest line is checked against them).
    pub descriptor_bytes: Vec<u8>,
    pub manifest: Option<String>,
    pub certificate: Option<Vec<u8>>,
}

/// Why a package could not be opened or checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    /// A file error.
    Io(String),
    /// No `.ovf` in the archive, or one that cannot be read
    /// ("This appliance does not contain a valid OVF file.").
    NoDescriptor(String),
    /// The archive is not a tar archive.
    NotOva,
    /// A file differs from the manifest ("The file {0} does not match the
    /// original.").
    Mismatch(String),
    /// An archive entry the manifest does not list.
    NotListed(String),
    /// A file the manifest lists is missing.
    Missing(String),
    /// A manifest line that cannot be read, or an unknown algorithm.
    BadManifest(String),
    /// No certificate, or one that cannot be read.
    BadCertificate(String),
    /// The signature does not match.
    BadSignature,
    Cancelled,
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::NoDescriptor(e) => write!(f, "no valid OVF descriptor: {e}"),
            Self::NotOva => f.write_str("not an OVA archive"),
            Self::Mismatch(n) => write!(f, "{n} does not match the manifest"),
            Self::NotListed(n) => write!(f, "{n} is not listed in the manifest"),
            Self::Missing(n) => write!(f, "{n} is listed in the manifest but missing"),
            Self::BadManifest(l) => write!(f, "a bad manifest line: {l}"),
            Self::BadCertificate(e) => write!(f, "a bad certificate: {e}"),
            Self::BadSignature => f.write_str("the signature does not match"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for PackageError {}

fn io(e: impl std::fmt::Display) -> PackageError {
    PackageError::Io(e.to_string())
}

/// A file of the package opened for reading: the reader positioned at its
/// start and its length.
pub struct PackageFile {
    pub file: std::fs::File,
    pub offset: u64,
    pub len: u64,
}

impl PackageFile {
    /// The file's bytes only.
    pub fn reader(mut self) -> std::io::Result<std::io::Take<std::fs::File>> {
        self.file.seek(SeekFrom::Start(self.offset))?;
        Ok(self.file.take(self.len))
    }
}

/// One line of a manifest: `ALG(name)= hex`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    pub algorithm: String,
    pub name: String,
    pub hex: String,
}

/// `FileDigest`: reads a manifest line.
pub fn parse_line(line: &str) -> Option<Digest> {
    let open = line.find('(')?;
    let close = line.rfind(")=")?;
    let algorithm = line[..open].trim().to_owned();
    let name = line[open + 1..close].to_owned();
    let hex = line[close + 2..].trim().to_lowercase();
    if algorithm.is_empty() || name.is_empty() || hex.is_empty() {
        return None;
    }
    Some(Digest {
        algorithm,
        name,
        hex,
    })
}

/// The `ring` digest of a manifest algorithm name.
pub fn algorithm(name: &str) -> Option<&'static ring::digest::Algorithm> {
    Some(match name.to_uppercase().replace('-', "").as_str() {
        "SHA1" => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        "SHA256" => &ring::digest::SHA256,
        "SHA384" => &ring::digest::SHA384,
        "SHA512" => &ring::digest::SHA512,
        _ => return None,
    })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The manifest line of `name` with its SHA-256 `digest`.
pub fn manifest_line(name: &str, digest: &[u8]) -> String {
    format!("SHA256({name})= {}", hex(digest))
}

impl Package {
    /// Opens the `.ovf` or `.ova` at `path`.
    pub fn open(path: &Path) -> Result<Self, PackageError> {
        let lower = path.to_string_lossy().to_lowercase();
        if lower.ends_with(".ovf") {
            let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            let bytes = std::fs::read(path).map_err(io)?;
            let descriptor = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = descriptor[..descriptor.len() - 4].to_owned();
            let manifest = std::fs::read_to_string(dir.join(format!("{name}.mf"))).ok();
            let certificate = std::fs::read(dir.join(format!("{name}.cert"))).ok();
            let envelope = parse(&bytes)?;
            return Ok(Self {
                path: path.to_path_buf(),
                descriptor,
                name,
                layout: Layout::Folder(dir),
                envelope,
                descriptor_bytes: bytes,
                manifest,
                certificate,
            });
        }
        let entries = ova::index(path).map_err(|e| match e {
            ova::OvaError::NotTar => PackageError::NotOva,
            ova::OvaError::Io(e) => io(e),
        })?;
        let Some(desc) = entries
            .iter()
            .find(|e| e.name.to_lowercase().ends_with(".ovf"))
            .cloned()
        else {
            return Err(PackageError::NoDescriptor(
                "the archive holds no .ovf file".into(),
            ));
        };
        let read = |e: &OvaEntry| -> Result<Vec<u8>, PackageError> {
            let mut f = std::fs::File::open(path).map_err(io)?;
            f.seek(SeekFrom::Start(e.offset)).map_err(io)?;
            let mut b = Vec::new();
            f.take(e.size).read_to_end(&mut b).map_err(io)?;
            Ok(b)
        };
        let bytes = read(&desc)?;
        let base = desc
            .name
            .rsplit('/')
            .next()
            .unwrap_or(&desc.name)
            .to_owned();
        let name = base[..base.len() - 4].to_owned();
        let find = |ext: &str| {
            entries
                .iter()
                .find(|e| e.name.rsplit('/').next() == Some(&format!("{name}.{ext}")))
                .cloned()
        };
        let manifest = match find("mf") {
            Some(e) => Some(String::from_utf8_lossy(&read(&e)?).into_owned()),
            None => None,
        };
        let certificate = match find("cert") {
            Some(e) => Some(read(&e)?),
            None => None,
        };
        let envelope = parse(&bytes)?;
        Ok(Self {
            path: path.to_path_buf(),
            descriptor: desc.name.clone(),
            name,
            layout: Layout::Ova(path.to_path_buf(), entries),
            envelope,
            descriptor_bytes: bytes,
            manifest,
            certificate,
        })
    }

    /// A package made of `envelope` whose files are local paths (a disk
    /// image import).
    pub fn image(path: &Path, envelope: Envelope) -> Self {
        Self {
            path: path.to_path_buf(),
            descriptor: String::new(),
            name: envelope.name.clone(),
            layout: Layout::Folder(path.parent().unwrap_or(Path::new(".")).to_path_buf()),
            envelope,
            descriptor_bytes: Vec::new(),
            manifest: None,
            certificate: None,
        }
    }

    fn entry(&self, href: &str) -> Option<&OvaEntry> {
        match &self.layout {
            Layout::Ova(_, entries) => entries
                .iter()
                .find(|e| e.name == href || e.name.rsplit('/').next() == href.rsplit('/').next()),
            Layout::Folder(_) => None,
        }
    }

    fn folder_path(&self, href: &str) -> Option<PathBuf> {
        match &self.layout {
            Layout::Folder(dir) => Some(if Path::new(href).is_absolute() {
                PathBuf::from(href)
            } else {
                dir.join(href)
            }),
            Layout::Ova(..) => None,
        }
    }

    /// The package holds `href`.
    pub fn has_file(&self, href: &str) -> bool {
        match &self.layout {
            Layout::Ova(..) => self.entry(href).is_some(),
            Layout::Folder(_) => self.folder_path(href).is_some_and(|p| p.is_file()),
        }
    }

    /// Opens `href`.
    pub fn open_file(&self, href: &str) -> Result<PackageFile, PackageError> {
        match &self.layout {
            Layout::Ova(path, _) => {
                let e = self
                    .entry(href)
                    .ok_or_else(|| PackageError::Missing(href.to_owned()))?;
                Ok(PackageFile {
                    file: std::fs::File::open(path).map_err(io)?,
                    offset: e.offset,
                    len: e.size,
                })
            }
            Layout::Folder(_) => {
                let p = self.folder_path(href).expect("a folder");
                let file =
                    std::fs::File::open(&p).map_err(|_| PackageError::Missing(href.to_owned()))?;
                let len = file.metadata().map_err(io)?.len();
                Ok(PackageFile {
                    file,
                    offset: 0,
                    len,
                })
            }
        }
    }

    pub fn has_manifest(&self) -> bool {
        self.manifest.is_some()
    }

    pub fn has_signature(&self) -> bool {
        self.certificate.is_some()
    }

    /// `VerifyManifest`. `progress` is told the bytes hashed and the total;
    /// `cancelled` is asked between pieces.
    pub fn verify_manifest(
        &self,
        progress: &dyn Fn(u64, u64),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), PackageError> {
        let text = self
            .manifest
            .as_deref()
            .ok_or_else(|| PackageError::BadManifest("no manifest".into()))?;
        let mut lines = Vec::new();
        for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let d = parse_line(l).ok_or_else(|| PackageError::BadManifest(l.to_owned()))?;
            if algorithm(&d.algorithm).is_none() {
                return Err(PackageError::BadManifest(l.to_owned()));
            }
            lines.push(d);
        }
        if let Layout::Ova(_, entries) = &self.layout {
            for e in entries {
                let base = e.name.rsplit('/').next().unwrap_or(&e.name);
                let lower = base.to_lowercase();
                if lower.ends_with(".mf") || lower.ends_with(".cert") {
                    continue;
                }
                if !lines.iter().any(|d| d.name == base || d.name == e.name) {
                    return Err(PackageError::NotListed(base.to_owned()));
                }
            }
        }
        let mut sizes = Vec::new();
        for d in &lines {
            let f = self
                .open_file(&d.name)
                .map_err(|_| PackageError::Missing(d.name.clone()))?;
            sizes.push(f.len);
        }
        let total: u64 = sizes.iter().sum();
        let mut done = 0u64;
        let mut buf = vec![0u8; 1 << 20];
        for d in &lines {
            let alg = algorithm(&d.algorithm).expect("checked");
            let mut ctx = ring::digest::Context::new(alg);
            let mut r = self.open_file(&d.name)?.reader().map_err(io)?;
            loop {
                if cancelled() {
                    return Err(PackageError::Cancelled);
                }
                let n = r.read(&mut buf).map_err(io)?;
                if n == 0 {
                    break;
                }
                ctx.update(&buf[..n]);
                done += n as u64;
                progress(done, total);
            }
            if hex(ctx.finish().as_ref()) != d.hex {
                return Err(PackageError::Mismatch(d.name.clone()));
            }
        }
        Ok(())
    }

    /// `VerifySignature`: the certificate's key signed the manifest.
    pub fn verify_signature(&self) -> Result<Signature, PackageError> {
        let cert = self
            .certificate
            .as_deref()
            .ok_or_else(|| PackageError::BadCertificate("no certificate".into()))?;
        let manifest = self
            .manifest
            .as_deref()
            .ok_or_else(|| PackageError::BadManifest("no manifest".into()))?;
        verify_signature(cert, manifest.as_bytes())
    }
}

fn parse(bytes: &[u8]) -> Result<Envelope, PackageError> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    Envelope::parse(text).map_err(|e| PackageError::NoDescriptor(e.0))
}

// ---- the signature -------------------------------------------------------------------------

/// A checked signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The certificate's subject, as text (`CN=…`).
    pub subject: String,
    /// The manifest digest algorithm of the signature.
    pub algorithm: String,
    /// Always false: the certificate's chain is not checked.
    pub trust_checked: bool,
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// One DER element: (tag, content, rest).
fn der(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *b.first()?;
    let first = *b.get(1)?;
    let (len, head) = if first < 0x80 {
        (usize::from(first), 2)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | usize::from(*b.get(2 + i)?);
        }
        (len, 2 + n)
    };
    let content = b.get(head..head + len)?;
    Some((tag, content, &b[head + len..]))
}

/// The DER certificate inside what the `.cert` holds: DER already, or the
/// Windows serialised certificate (property 32 holds the DER).
fn certificate_der(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.first() == Some(&0x30) {
        return Some(raw.to_vec());
    }
    let mut rest = raw;
    while rest.len() >= 12 {
        let id = u32::from_le_bytes(rest[..4].try_into().ok()?);
        let len = u32::from_le_bytes(rest[8..12].try_into().ok()?) as usize;
        let data = rest.get(12..12 + len)?;
        if id == 32 {
            return Some(data.to_vec());
        }
        rest = &rest[12 + len..];
    }
    None
}

/// The certificate's subject and RSA public key (PKCS#1 DER).
fn certificate_key(cert: &[u8]) -> Option<(String, Vec<u8>)> {
    let (_, body, _) = der(cert)?;
    let (_, tbs, _) = der(body)?;
    let mut rest = tbs;
    let (tag, _, r) = der(rest)?;
    if tag == 0xa0 {
        rest = r; // version
    }
    let (_, _, r) = der(rest)?; // serial
    let (_, _, r) = der(r)?; // signature algorithm
    let (_, _, r) = der(r)?; // issuer
    let (_, _, r) = der(r)?; // validity
    let (_, subject, r) = der(r)?;
    let (_, spki, _) = der(r)?;
    let (_, _, r) = der(spki)?; // algorithm
    let (tag, bits, _) = der(r)?;
    if tag != 0x03 || bits.first() != Some(&0) {
        return None;
    }
    // The subject's common name, when there is one.
    let mut cn = String::new();
    let mut s = subject;
    while let Some((_, set, next)) = der(s) {
        if let Some((_, seq, _)) = der(set)
            && let Some((_, oid, r)) = der(seq)
            && oid == [0x55, 0x04, 0x03]
            && let Some((_, value, _)) = der(r)
        {
            cn = format!("CN={}", String::from_utf8_lossy(value));
        }
        s = next;
    }
    Some((cn, bits[1..].to_vec()))
}

/// The subject of the certificate in a `.cert` file ("View Certificate"),
/// `None` when there is no readable RSA certificate.
pub fn certificate_subject(cert_file: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(cert_file);
    let pem = text
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)?
        .split("-----END CERTIFICATE-----")
        .next()?;
    let der = certificate_der(&base64(pem)?)?;
    certificate_key(&der).map(|(subject, _)| subject)
}

/// Checks a `.cert` file against the manifest's bytes.
pub fn verify_signature(cert_file: &[u8], manifest: &[u8]) -> Result<Signature, PackageError> {
    let text = String::from_utf8_lossy(cert_file);
    let first = text.lines().next().unwrap_or_default();
    let digest = parse_line(first)
        .ok_or_else(|| PackageError::BadCertificate("no signature line".into()))?;
    let signature = unhex(&digest.hex)
        .ok_or_else(|| PackageError::BadCertificate("the signature is not hex".into()))?;
    let pem = text
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)
        .and_then(|r| r.split("-----END CERTIFICATE-----").next())
        .ok_or_else(|| PackageError::BadCertificate("no certificate".into()))?;
    let raw = base64(pem)
        .ok_or_else(|| PackageError::BadCertificate("the certificate is not base64".into()))?;
    let der = certificate_der(&raw)
        .ok_or_else(|| PackageError::BadCertificate("no certificate in the file".into()))?;
    let (subject, key) = certificate_key(&der)
        .ok_or_else(|| PackageError::BadCertificate("not an RSA certificate".into()))?;
    use ring::signature::{
        RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY, RSA_PKCS1_2048_8192_SHA256,
        RSA_PKCS1_2048_8192_SHA384, RSA_PKCS1_2048_8192_SHA512, UnparsedPublicKey,
        VerificationAlgorithm,
    };
    let alg = digest.algorithm.to_uppercase().replace('-', "");
    let (scheme, hash): (&dyn VerificationAlgorithm, _) = match alg.as_str() {
        "SHA1" => (
            &RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY,
            &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        ),
        "SHA256" => (&RSA_PKCS1_2048_8192_SHA256, &ring::digest::SHA256),
        "SHA384" => (&RSA_PKCS1_2048_8192_SHA384, &ring::digest::SHA384),
        "SHA512" => (&RSA_PKCS1_2048_8192_SHA512, &ring::digest::SHA512),
        _ => return Err(PackageError::BadCertificate(digest.algorithm.clone())),
    };
    let key = UnparsedPublicKey::new(scheme, key);
    let once = ring::digest::digest(hash, manifest);
    let ok = key.verify(manifest, &signature).is_ok()
        // The original console signs the manifest's hash (SHA-256).
        || (alg != "SHA1" && key.verify(once.as_ref(), &signature).is_ok());
    if !ok {
        return Err(PackageError::BadSignature);
    }
    Ok(Signature {
        subject,
        algorithm: digest.algorithm,
        trust_checked: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ova::TarWriter;

    fn sha256(b: &[u8]) -> Vec<u8> {
        ring::digest::digest(&ring::digest::SHA256, b)
            .as_ref()
            .to_vec()
    }

    const DESCRIPTOR: &str = r#"<?xml version="1.0"?><Envelope ovf:version="1.0.0" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/1"><References><File ovf:id="f1" ovf:href="disk.vhd"/></References></Envelope>"#;

    fn ova(dir: &Path, manifest: &str, extra: bool) -> PathBuf {
        let p = dir.join("pkg.ova");
        let mut t = TarWriter::new(std::fs::File::create(&p).unwrap());
        t.entry("pkg.ovf", DESCRIPTOR.as_bytes()).unwrap();
        t.entry("disk.vhd", b"disk bytes").unwrap();
        if extra {
            t.entry("notes.txt", b"x").unwrap();
        }
        t.entry("pkg.mf", manifest.as_bytes()).unwrap();
        t.finish().unwrap();
        p
    }

    fn good_manifest() -> String {
        format!(
            "{}\n{}\n",
            manifest_line("pkg.ovf", &sha256(DESCRIPTOR.as_bytes())),
            manifest_line("disk.vhd", &sha256(b"disk bytes"))
        )
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xen-pkg-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ova_manifests() {
        let d = dir("ova");
        let p = ova(&d, &good_manifest(), false);
        let pkg = Package::open(&p).unwrap();
        assert_eq!(pkg.name, "pkg");
        assert!(pkg.has_manifest() && !pkg.has_signature());
        assert!(pkg.has_file("disk.vhd"));
        let mut f = pkg.open_file("disk.vhd").unwrap().reader().unwrap();
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        assert_eq!(s, "disk bytes");
        pkg.verify_manifest(&|_, _| {}, &|| false).unwrap();
        // A wrong digest.
        let bad = good_manifest().replace(&hex(&sha256(b"disk bytes")), &"0".repeat(64));
        let pkg = Package::open(&ova(&d, &bad, false)).unwrap();
        assert_eq!(
            pkg.verify_manifest(&|_, _| {}, &|| false),
            Err(PackageError::Mismatch("disk.vhd".into()))
        );
        // A file the manifest does not list.
        let pkg = Package::open(&ova(&d, &good_manifest(), true)).unwrap();
        assert_eq!(
            pkg.verify_manifest(&|_, _| {}, &|| false),
            Err(PackageError::NotListed("notes.txt".into()))
        );
        // A listed file that is missing.
        let more = format!("{}SHA1(gone.vhd)= 00\n", good_manifest());
        let pkg = Package::open(&ova(&d, &more, false)).unwrap();
        assert_eq!(
            pkg.verify_manifest(&|_, _| {}, &|| false),
            Err(PackageError::Missing("gone.vhd".into()))
        );
        // Garbage.
        let pkg = Package::open(&ova(&d, "nonsense\n", false)).unwrap();
        assert!(matches!(
            pkg.verify_manifest(&|_, _| {}, &|| false),
            Err(PackageError::BadManifest(_))
        ));
        // Cancelled.
        let pkg = Package::open(&ova(&d, &good_manifest(), false)).unwrap();
        assert_eq!(
            pkg.verify_manifest(&|_, _| {}, &|| true),
            Err(PackageError::Cancelled)
        );
    }

    #[test]
    fn folders() {
        let d = dir("folder");
        std::fs::write(d.join("pkg.ovf"), DESCRIPTOR).unwrap();
        std::fs::write(d.join("disk.vhd"), b"disk bytes").unwrap();
        std::fs::write(d.join("pkg.mf"), good_manifest()).unwrap();
        let pkg = Package::open(&d.join("pkg.ovf")).unwrap();
        assert!(pkg.has_file("disk.vhd") && !pkg.has_file("other.vhd"));
        pkg.verify_manifest(&|_, _| {}, &|| false).unwrap();
        std::fs::write(d.join("disk.vhd"), b"disk bytez").unwrap();
        assert_eq!(
            pkg.verify_manifest(&|_, _| {}, &|| false),
            Err(PackageError::Mismatch("disk.vhd".into()))
        );
        std::fs::write(d.join("broken.ovf"), "<nope").unwrap();
        assert!(matches!(
            Package::open(&d.join("broken.ovf")),
            Err(PackageError::NoDescriptor(_))
        ));
    }

    fn cert_file(manifest: &[u8], double: bool, serialized: bool, tamper: bool) -> Vec<u8> {
        use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
        let key = RsaKeyPair::from_pkcs8(include_bytes!("testdata/ovf-sign-key.pk8")).unwrap();
        let data = if double {
            sha256(manifest)
        } else {
            manifest.to_vec()
        };
        let mut sig = vec![0u8; key.public().modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            &data,
            &mut sig,
        )
        .unwrap();
        if tamper {
            sig[10] ^= 1;
        }
        let pem = include_str!("testdata/ovf-sign-cert.pem");
        let der_b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let body = if serialized {
            // The Windows serialised form: a property, then the DER (32).
            let der = base64(&der_b64).unwrap();
            let mut s = Vec::new();
            for (id, data) in [(3u32, vec![1u8; 20]), (32u32, der)] {
                s.extend_from_slice(&id.to_le_bytes());
                s.extend_from_slice(&1u32.to_le_bytes());
                s.extend_from_slice(&(data.len() as u32).to_le_bytes());
                s.extend_from_slice(&data);
            }
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for c in s.chunks(3) {
                let n = (u32::from(c[0]) << 16)
                    | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                    | u32::from(*c.get(2).unwrap_or(&0));
                for i in 0..4 {
                    if i <= c.len() {
                        out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                    } else {
                        out.push('=');
                    }
                }
            }
            out
        } else {
            der_b64
        };
        format!(
            "SHA256(pkg.mf)= {}\n-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n",
            hex(&sig)
        )
        .into_bytes()
    }

    #[test]
    fn signatures() {
        let mf = good_manifest();
        for (double, serialized) in [(false, false), (true, false), (true, true)] {
            let s = verify_signature(
                &cert_file(mf.as_bytes(), double, serialized, false),
                mf.as_bytes(),
            )
            .unwrap();
            assert_eq!(s.subject, "CN=xenadmin-rs-test-signer");
            assert_eq!(
                certificate_subject(&cert_file(mf.as_bytes(), double, serialized, false))
                    .as_deref(),
                Some("CN=xenadmin-rs-test-signer")
            );
            assert!(!s.trust_checked);
        }
        assert_eq!(
            verify_signature(&cert_file(mf.as_bytes(), false, false, true), mf.as_bytes()),
            Err(PackageError::BadSignature)
        );
        // Another manifest.
        assert_eq!(
            verify_signature(&cert_file(b"other", false, false, false), mf.as_bytes()),
            Err(PackageError::BadSignature)
        );
        assert!(matches!(
            verify_signature(b"garbage", mf.as_bytes()),
            Err(PackageError::BadCertificate(_))
        ));
    }

    #[test]
    fn manifest_lines() {
        assert_eq!(
            parse_line("SHA256(a (1).vhd)= ABcd"),
            Some(Digest {
                algorithm: "SHA256".into(),
                name: "a (1).vhd".into(),
                hex: "abcd".into()
            })
        );
        assert_eq!(parse_line("nothing"), None);
        assert!(algorithm("sha-512").is_some() && algorithm("md5").is_none());
    }
}
