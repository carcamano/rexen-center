//! The release signature: a [minisign] signature over the release's
//! `SHA256SUMS`, published beside it as `SHA256SUMS.minisig`, made with the
//! project's release-signing key and checked against the public keys built
//! into this program ([`RELEASE_KEYS`]).
//!
//! What this proves: the checksum file, and so every package it lists, is
//! the one the holder of a trusted release key signed. Unlike the checksums
//! alone, it holds when the release page itself is changed (a stolen
//! account or token can replace a release's files, but not sign them).
//! What it does not: it is not the operating system's code signing (macOS
//! Gatekeeper and Windows SmartScreen see the packages exactly as before),
//! and it says nothing about a release signed with a key that was stolen.
//!
//! The format is minisign's, so the signature can be made and checked with
//! the `minisign` tool: in its *legacy* mode (`minisign -S -l`), an Ed25519
//! signature over the file's bytes, which `ring` checks as it is. The
//! default mode of recent minisign versions signs a BLAKE2b hash of the file
//! instead; it is refused with a message that says to sign with `-l`
//! (`packaging/sign-release.sh` does). The signature's trusted comment is
//! signed too (minisign's "global signature") and checked.
//!
//! [minisign]: https://jedisct1.github.io/minisign/

use ring::signature::{ED25519, UnparsedPublicKey};

/// The signature of [`crate::release::CHECKSUMS_ASSET`], as `minisign`
/// names it by default.
pub const SIGNATURE_ASSET: &str = "SHA256SUMS.minisig";

/// The release-signing public keys this program trusts, in minisign's
/// format (the second line of a `minisign.pub` file). A release is trusted
/// when one of them signed its `SHA256SUMS`; the signature names its key by
/// the key id, so more than one key can be trusted at a time (a rotation
/// ships the new key in a release signed with the old one; see
/// `packaging/release-signing/README.md`).
///
/// Each key's file is in `packaging/release-signing/` too (a test keeps
/// them the same).
pub const RELEASE_KEYS: &[&str] = &[
    // Key 1, id 4B93885D9D72AAC9, created 2026-11-27
    // (packaging/release-signing/rexen-release-1.pub).
    "RWTJqnKdXYiTS8U7nG6cjVGIH6sRYUZ09F+rVLOHHTzUmtAjTz3Oqc9k",
];

/// minisign's algorithm tag of a public key and of a legacy signature
/// (Ed25519 over the message itself).
const ALG_ED25519: [u8; 2] = *b"Ed";

/// minisign's tag of a signature over the message's BLAKE2b-512 hash.
const ALG_PREHASHED: [u8; 2] = *b"ED";

/// A trusted public key.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicKey {
    id: [u8; 8],
    key: [u8; 32],
}

impl PublicKey {
    /// A key in minisign's format: base64 of `Ed`, the 8-byte key id and
    /// the 32-byte Ed25519 key.
    pub fn parse(text: &str) -> Result<Self, SignatureError> {
        let bytes = base64_decode(text.trim())
            .ok_or(SignatureError::Malformed("the public key is not base64"))?;
        if bytes.len() != 42 || bytes[..2] != ALG_ED25519 {
            return Err(SignatureError::Malformed(
                "the public key is not a minisign Ed25519 key",
            ));
        }
        let mut id = [0u8; 8];
        let mut key = [0u8; 32];
        id.copy_from_slice(&bytes[2..10]);
        key.copy_from_slice(&bytes[10..]);
        Ok(Self { id, key })
    }

    /// The key id as minisign prints it (16 hex digits, upper case).
    pub fn id(&self) -> String {
        key_id_hex(&self.id)
    }

    /// Builds a key from its parts (the tests' signing key).
    #[cfg(any(test, feature = "testing"))]
    pub fn from_parts(id: [u8; 8], key: [u8; 32]) -> Self {
        Self { id, key }
    }

    /// The key in minisign's format.
    #[cfg(any(test, feature = "testing"))]
    pub fn to_minisign(&self) -> String {
        let mut bytes = ALG_ED25519.to_vec();
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(&self.key);
        base64_encode(&bytes)
    }
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey({})", self.id())
    }
}

/// The keys of [`RELEASE_KEYS`].
pub fn release_keys() -> Vec<PublicKey> {
    RELEASE_KEYS
        .iter()
        .map(|k| PublicKey::parse(k).expect("a built-in release key is a minisign public key"))
        .collect()
}

/// A signature file, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureFile {
    algorithm: [u8; 2],
    key_id: [u8; 8],
    signature: [u8; 64],
    trusted_comment: String,
    global_signature: [u8; 64],
}

impl SignatureFile {
    /// minisign's four lines: `untrusted comment: …`, the signature,
    /// `trusted comment: …` and the global signature.
    pub fn parse(text: &str) -> Result<Self, SignatureError> {
        let mut lines = text
            .lines()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.is_empty());
        let malformed = SignatureError::Malformed;
        let untrusted = lines
            .next()
            .ok_or(malformed("the signature file is empty"))?;
        if !untrusted.starts_with("untrusted comment:") {
            return Err(malformed("the signature file is not a minisign signature"));
        }
        let sig = lines
            .next()
            .and_then(base64_decode)
            .filter(|b| b.len() == 74)
            .ok_or(malformed("the signature line is not a minisign signature"))?;
        let trusted_comment = lines
            .next()
            .and_then(|l| l.strip_prefix("trusted comment: "))
            .ok_or(malformed("the signature file has no trusted comment"))?
            .to_owned();
        let global = lines
            .next()
            .and_then(base64_decode)
            .filter(|b| b.len() == 64)
            .ok_or(malformed("the signature file has no global signature"))?;
        if lines.next().is_some() {
            return Err(malformed("the signature file has extra lines"));
        }
        let mut out = Self {
            algorithm: [sig[0], sig[1]],
            key_id: [0; 8],
            signature: [0; 64],
            trusted_comment,
            global_signature: [0; 64],
        };
        out.key_id.copy_from_slice(&sig[2..10]);
        out.signature.copy_from_slice(&sig[10..]);
        out.global_signature.copy_from_slice(&global);
        Ok(out)
    }

    /// The id of the key that made it.
    pub fn key_id(&self) -> String {
        key_id_hex(&self.key_id)
    }

    pub fn trusted_comment(&self) -> &str {
        &self.trusted_comment
    }
}

/// A checked signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The id of the trusted key that made it.
    pub key_id: String,
    pub trusted_comment: String,
}

/// Why a signature was refused.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SignatureError {
    #[error("{0}")]
    Malformed(&'static str),
    #[error(
        "the signature was made in minisign's pre-hashed mode, which is not accepted (sign with `minisign -S -l`)"
    )]
    Prehashed,
    #[error("the signature was made with key {0}, which this version does not trust")]
    UnknownKey(String),
    #[error("the signature does not match the checksum file")]
    Invalid,
    #[error("the signature's trusted comment was altered")]
    CommentAltered,
}

/// Checks `signature` (a `.minisig` file's text) over `message` (the exact
/// bytes of the signed file) against `keys`.
pub fn verify(
    message: &[u8],
    signature: &str,
    keys: &[PublicKey],
) -> Result<Verified, SignatureError> {
    let file = SignatureFile::parse(signature)?;
    match file.algorithm {
        ALG_ED25519 => {}
        ALG_PREHASHED => return Err(SignatureError::Prehashed),
        _ => return Err(SignatureError::Malformed("unknown signature algorithm")),
    }
    let key = keys
        .iter()
        .find(|k| k.id == file.key_id)
        .ok_or_else(|| SignatureError::UnknownKey(file.key_id()))?;
    let public = UnparsedPublicKey::new(&ED25519, &key.key);
    public
        .verify(message, &file.signature)
        .map_err(|_| SignatureError::Invalid)?;
    // The global signature binds the trusted comment to this signature.
    let mut global = file.signature.to_vec();
    global.extend_from_slice(file.trusted_comment.as_bytes());
    public
        .verify(&global, &file.global_signature)
        .map_err(|_| SignatureError::CommentAltered)?;
    Ok(Verified {
        key_id: key.id(),
        trusted_comment: file.trusted_comment,
    })
}

/// minisign shows key ids as the little-endian number they store.
fn key_id_hex(id: &[u8; 8]) -> String {
    id.iter().rev().map(|b| format!("{b:02X}")).collect()
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (what minisign writes); `None` for anything
/// else.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if text.is_empty() || !text.len().is_multiple_of(4) {
        return None;
    }
    let pad = text.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc: u32 = 0;
    let body = &text[..text.len() - pad];
    for (i, &c) in body.iter().enumerate() {
        let v = BASE64.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | v;
        if i % 4 == 3 {
            out.extend_from_slice(&acc.to_be_bytes()[1..]);
            acc = 0;
        }
    }
    match body.len() % 4 {
        0 => {}
        2 => {
            // 12 bits, the last 4 must be zero.
            if acc & 0xf != 0 {
                return None;
            }
            out.push((acc >> 4) as u8);
        }
        3 => {
            // 18 bits, the last 2 must be zero.
            if acc & 0x3 != 0 {
                return None;
            }
            out.extend_from_slice(&((acc >> 2) as u16).to_be_bytes());
        }
        _ => return None,
    }
    Some(out)
}

#[cfg(any(test, feature = "testing"))]
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut buf = [0u8; 3];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u32::from_be_bytes([0, buf[0], buf[1], buf[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A signing key for the tests, from a fixed seed: never one of
/// [`RELEASE_KEYS`], so what it signs is refused by a real build.
#[cfg(any(test, feature = "testing"))]
pub struct TestKey {
    pair: ring::signature::Ed25519KeyPair,
    id: [u8; 8],
}

#[cfg(any(test, feature = "testing"))]
impl TestKey {
    /// The key made from `seed` (any byte; different seeds, different keys).
    pub fn new(seed: u8) -> Self {
        use ring::signature::KeyPair;
        let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[seed; 32])
            .expect("an Ed25519 seed");
        let mut id = [0u8; 8];
        id.copy_from_slice(&pair.public_key().as_ref()[..8]);
        Self { pair, id }
    }

    /// The tests' trusted key.
    pub fn trusted() -> Self {
        Self::new(7)
    }

    pub fn public(&self) -> PublicKey {
        use ring::signature::KeyPair;
        let mut key = [0u8; 32];
        key.copy_from_slice(self.pair.public_key().as_ref());
        PublicKey::from_parts(self.id, key)
    }

    /// A `.minisig` file over `message`, as `minisign -S -l -t <comment>`
    /// writes it.
    pub fn sign(&self, message: &[u8], trusted_comment: &str) -> String {
        let sig = self.pair.sign(message);
        let mut line = ALG_ED25519.to_vec();
        line.extend_from_slice(&self.id);
        line.extend_from_slice(sig.as_ref());
        let mut global = sig.as_ref().to_vec();
        global.extend_from_slice(trusted_comment.as_bytes());
        let global = self.pair.sign(&global);
        format!(
            "untrusted comment: signature from the tests' key\n{}\ntrusted comment: {trusted_comment}\n{}\n",
            base64_encode(&line),
            base64_encode(global.as_ref())
        )
    }
}

#[cfg(test)]
mod tests;
