//! SHA-256 verification of a download against the release's `SHA256SUMS`
//! (the output of `sha256sum`: `<64 hex digits>  <file name>` per line, a
//! `*` before the name in binary mode).
//!
//! What this proves: the file is the one the release's checksum file names,
//! so a download cut short or changed in transit or on a mirror is refused.
//! What it does not: both files come from the same release page, so whoever
//! can replace a release's files can replace both (the packages are not
//! signed; see the README).

use std::io::Read;
use std::path::Path;

/// A SHA-256 digest.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Sha256(pub [u8; 32]);

impl Sha256 {
    /// Parses 64 hex digits (any case).
    pub fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(out))
    }

    /// The digest of a file, read in pieces.
    pub fn of_file(path: &Path) -> std::io::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            context.update(&buf[..n]);
        }
        Ok(Self::from_digest(context.finish()))
    }

    /// The digest of `bytes`.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self::from_digest(ring::digest::digest(&ring::digest::SHA256, bytes))
    }

    fn from_digest(digest: ring::digest::Digest) -> Self {
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        Self(out)
    }
}

impl std::fmt::Display for Sha256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sha256({self})")
    }
}

/// Why a download was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChecksumError {
    #[error("the release has no checksum file")]
    NoChecksumFile,
    #[error("the checksum file has no entry for {0}")]
    NotListed(String),
    #[error("the checksum file lists {0} more than once")]
    Ambiguous(String),
    #[error("the checksum file is malformed (line {0})")]
    Malformed(usize),
    #[error("{name} does not match its checksum (expected {expected}, got {actual})")]
    Mismatch {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("cannot read the download: {0}")]
    Io(String),
}

/// The digest `sums` gives for `name`. Every line must be well formed, and
/// `name` listed exactly once.
pub fn expected_digest(sums: &str, name: &str) -> Result<Sha256, ChecksumError> {
    let mut found = None;
    for (i, line) in sums.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let malformed = || ChecksumError::Malformed(i + 1);
        let (hex, rest) = line.split_once(' ').ok_or_else(malformed)?;
        let digest = Sha256::parse_hex(hex).ok_or_else(malformed)?;
        // "  name" (text mode) or " *name" (binary mode).
        let file = rest
            .strip_prefix(' ')
            .or_else(|| rest.strip_prefix('*'))
            .ok_or_else(malformed)?;
        if file.is_empty() {
            return Err(malformed());
        }
        if file == name {
            if found.is_some() {
                return Err(ChecksumError::Ambiguous(name.to_owned()));
            }
            found = Some(digest);
        }
    }
    found.ok_or_else(|| ChecksumError::NotListed(name.to_owned()))
}

/// Checks the file at `path`, downloaded as `name`, against `sums`.
pub fn verify_file(path: &Path, name: &str, sums: &str) -> Result<Sha256, ChecksumError> {
    let expected = expected_digest(sums, name)?;
    let actual = Sha256::of_file(path).map_err(|e| ChecksumError::Io(e.to_string()))?;
    // Both digests are public: a plain comparison will do.
    if expected != actual {
        return Err(ChecksumError::Mismatch {
            name: name.to_owned(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn digests() {
        assert_eq!(Sha256::of_bytes(b"").to_string(), EMPTY);
        assert_eq!(
            Sha256::of_bytes(b"abc").to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            Sha256::parse_hex(&EMPTY.to_uppercase()),
            Some(Sha256::of_bytes(b""))
        );
        assert_eq!(Sha256::parse_hex("abc"), None);
        assert_eq!(Sha256::parse_hex(&"g".repeat(64)), None);
    }

    #[test]
    fn reads_sha256sum_output() {
        let abc = Sha256::of_bytes(b"abc");
        let sums = format!(
            "{EMPTY}  rexen-center-0.2.0-macos-arm64.dmg\r\n{abc} *rexen-center_0.2.0-1_amd64.deb\n\n"
        );
        assert_eq!(
            expected_digest(&sums, "rexen-center-0.2.0-macos-arm64.dmg"),
            Ok(Sha256::of_bytes(b""))
        );
        assert_eq!(
            expected_digest(&sums, "rexen-center_0.2.0-1_amd64.deb"),
            Ok(abc)
        );
        assert_eq!(
            expected_digest(&sums, "rexen-center-0.2.0-windows-x64.msi"),
            Err(ChecksumError::NotListed(
                "rexen-center-0.2.0-windows-x64.msi".into()
            ))
        );
        // A name is a whole name, not a part of one.
        assert!(expected_digest(&sums, "macos-arm64.dmg").is_err());
    }

    #[test]
    fn refuses_malformed_and_ambiguous_files() {
        for (sums, line) in [
            ("not a checksum file\n".to_owned(), 1),
            (format!("{EMPTY}\n"), 1),
            (format!("{EMPTY}  \n"), 1),
            (format!("{}  x.dmg\n", &EMPTY[1..]), 1),
            (format!("{EMPTY}  a.dmg\n{EMPTY}x  b.dmg\n"), 2),
            ("<html>Not Found</html>".to_owned(), 1),
        ] {
            assert_eq!(
                expected_digest(&sums, "x.dmg"),
                Err(ChecksumError::Malformed(line)),
                "{sums}"
            );
        }
        let twice = format!("{EMPTY}  x.dmg\n{EMPTY}  x.dmg\n");
        assert_eq!(
            expected_digest(&twice, "x.dmg"),
            Err(ChecksumError::Ambiguous("x.dmg".into()))
        );
    }

    #[test]
    fn verifies_files() {
        let dir = std::env::temp_dir().join(format!("xen-update-sum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pkg.deb");
        std::fs::write(&file, b"abc").unwrap();
        let good = format!("{}  pkg.deb\n", Sha256::of_bytes(b"abc"));
        assert_eq!(
            verify_file(&file, "pkg.deb", &good),
            Ok(Sha256::of_bytes(b"abc"))
        );
        // One byte changed.
        std::fs::write(&file, b"abd").unwrap();
        let err = verify_file(&file, "pkg.deb", &good).unwrap_err();
        assert!(matches!(err, ChecksumError::Mismatch { .. }), "{err}");
        assert!(err.to_string().contains("does not match"), "{err}");
        // No entry, no file.
        assert!(matches!(
            verify_file(&file, "other.deb", &good),
            Err(ChecksumError::NotListed(_))
        ));
        assert!(matches!(
            verify_file(&dir.join("missing"), "pkg.deb", &good),
            Err(ChecksumError::Io(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
