//! The signature check against files the real `minisign` tool wrote
//! (minisign 0.12, a throwaway key made for these tests with
//! `minisign -G -W`), and against the tests' own signing key.

use super::*;

/// The throwaway key's public half (`minisign.pub`, key id 9894F660867DD46D).
const MINISIGN_KEY: &str = "RWRt1H2GYPaUmETZFMNEWPDf7LFtHzSyRb7Jwnoadf8Z5A6tXkFYgLbY";

/// The file it signed.
const SUMS: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  rexen-center-0.2.0-linux-x86_64.tar.gz\n";

/// `minisign -S -l -s t.key -m SHA256SUMS -t "rexen-center v0.2.0 SHA256SUMS"`
const LEGACY: &str = "untrusted comment: signature from minisign secret key
RWRt1H2GYPaUmHv2R3XYYZ5Bzn9HRUkZVW/O1ivJfXz0vf3EgDoPCpInvtzFQEjb2Gi6AcOrEnUYGwnvkwOvOVq401TUEnZ/iws=
trusted comment: rexen-center v0.2.0 SHA256SUMS
v5/nJkKd/hPDFPNBG/wNzGvg4N+0D0xeMIPHo9MTZIeVda83mdgu8/hePNn3S4DKAtpr6lQbo+ur729TFb2RAg==
";

/// The same without `-l`: minisign's default, pre-hashed mode.
const PREHASHED: &str = "untrusted comment: signature from minisign secret key
RURt1H2GYPaUmOiwrMbzvtYHWQ7fJl4zpZhSjSc29d20R58Hepr2CCHUBB2aaH/ZTx9Lo9iE84vaayWp/Z3jZjS1/X5pMSn79gk=
trusted comment: rexen-center v0.2.0 SHA256SUMS
jmERINVK8Sa2U+REjkHIfN9jQf1tr5hefV7jBNPvXeYH1TY7P2uGqSFxClRbxtTULHSmtPGmUq1edFFnQ4MIBw==
";

fn minisign_key() -> PublicKey {
    PublicKey::parse(MINISIGN_KEY).unwrap()
}

/// `text` with the base64 line `line` (0-based) decoded, changed by `f` and
/// encoded again.
fn alter_line(text: &str, line: usize, f: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut bytes = base64_decode(&lines[line]).unwrap();
    f(&mut bytes);
    lines[line] = base64_encode(&bytes);
    lines.join("\n") + "\n"
}

#[test]
fn a_signature_made_by_minisign_in_legacy_mode_is_accepted() {
    let key = minisign_key();
    assert_eq!(key.id(), "9894F660867DD46D");
    let verified = verify(SUMS.as_bytes(), LEGACY, &[key]).unwrap();
    assert_eq!(verified.key_id, "9894F660867DD46D");
    assert_eq!(verified.trusted_comment, "rexen-center v0.2.0 SHA256SUMS");
    // CRLF line ends (a file that went through Windows) read the same.
    let crlf = LEGACY.replace('\n', "\r\n");
    assert!(verify(SUMS.as_bytes(), &crlf, &[minisign_key()]).is_ok());
}

#[test]
fn the_pre_hashed_mode_is_refused_with_what_to_do() {
    let err = verify(SUMS.as_bytes(), PREHASHED, &[minisign_key()]).unwrap_err();
    assert_eq!(err, SignatureError::Prehashed);
    assert!(err.to_string().contains("minisign -S -l"), "{err}");
}

#[test]
fn a_tampered_checksum_file_is_refused() {
    let mut sums = SUMS.as_bytes().to_vec();
    sums[0] = b'f';
    assert_eq!(
        verify(&sums, LEGACY, &[minisign_key()]),
        Err(SignatureError::Invalid)
    );
    // A line added, or the final newline dropped.
    let more = format!("{SUMS}{SUMS}");
    assert_eq!(
        verify(more.as_bytes(), LEGACY, &[minisign_key()]),
        Err(SignatureError::Invalid)
    );
    assert_eq!(
        verify(SUMS.trim_end().as_bytes(), LEGACY, &[minisign_key()]),
        Err(SignatureError::Invalid)
    );
}

#[test]
fn a_tampered_signature_is_refused() {
    // One bit of the Ed25519 signature.
    let bad = alter_line(LEGACY, 1, |b| b[40] ^= 1);
    assert_eq!(
        verify(SUMS.as_bytes(), &bad, &[minisign_key()]),
        Err(SignatureError::Invalid)
    );
    // The trusted comment changed (it is signed too).
    let bad = LEGACY.replace("v0.2.0 SHA256SUMS", "v9.9.9 SHA256SUMS");
    assert_eq!(
        verify(SUMS.as_bytes(), &bad, &[minisign_key()]),
        Err(SignatureError::CommentAltered)
    );
    // The global signature changed.
    let bad = alter_line(LEGACY, 3, |b| b[0] ^= 1);
    assert_eq!(
        verify(SUMS.as_bytes(), &bad, &[minisign_key()]),
        Err(SignatureError::CommentAltered)
    );
    // The algorithm tag changed to something else.
    let bad = alter_line(LEGACY, 1, |b| b[1] = b'X');
    assert!(matches!(
        verify(SUMS.as_bytes(), &bad, &[minisign_key()]),
        Err(SignatureError::Malformed(_))
    ));
}

#[test]
fn a_signature_by_another_key_is_refused() {
    // The release keys do not include the throwaway one.
    assert_eq!(
        verify(SUMS.as_bytes(), LEGACY, &release_keys()),
        Err(SignatureError::UnknownKey("9894F660867DD46D".into()))
    );
    // Nor does an empty list trust anything.
    assert!(matches!(
        verify(SUMS.as_bytes(), LEGACY, &[]),
        Err(SignatureError::UnknownKey(_))
    ));
    // A key that claims the signer's id is not the signer's key.
    let mut impostor = minisign_key();
    impostor.key = TestKey::new(1).public().key;
    assert_eq!(
        verify(SUMS.as_bytes(), LEGACY, &[impostor]),
        Err(SignatureError::Invalid)
    );
    // The id in the signature changed to a trusted key's: still that key's
    // signature check, which fails.
    let other = TestKey::new(2).public();
    let bad = alter_line(LEGACY, 1, |b| b[2..10].copy_from_slice(&other.id));
    assert_eq!(
        verify(SUMS.as_bytes(), &bad, &[other]),
        Err(SignatureError::Invalid)
    );
}

#[test]
fn malformed_or_missing_signatures_are_refused() {
    for text in [
        "",
        "\n\n",
        "not a signature",
        "untrusted comment: x\n",
        "untrusted comment: x\nnot base64!\ntrusted comment: y\nAAAA\n",
        // Truncated after the signature line.
        &LEGACY.lines().take(2).collect::<Vec<_>>().join("\n"),
        // No "trusted comment: " prefix.
        &LEGACY.replace("trusted comment: ", "comment: "),
        // An extra line.
        &format!("{LEGACY}extra\n"),
        "<html>Not Found</html>",
    ] {
        assert!(
            matches!(
                verify(SUMS.as_bytes(), text, &[minisign_key()]),
                Err(SignatureError::Malformed(_))
            ),
            "{text:?}"
        );
    }
}

#[test]
fn rotation_trusts_every_key_in_the_list() {
    let old = TestKey::new(10);
    let new = TestKey::new(11);
    let both = [old.public(), new.public()];
    for key in [&old, &new] {
        let sig = key.sign(SUMS.as_bytes(), "rexen-center v0.3.0 SHA256SUMS");
        let verified = verify(SUMS.as_bytes(), &sig, &both).unwrap();
        assert_eq!(verified.key_id, key.public().id());
    }
    // Once the old key is dropped from the list, what it signs is refused.
    let by_old = old.sign(SUMS.as_bytes(), "c");
    assert_eq!(
        verify(SUMS.as_bytes(), &by_old, &[new.public()]),
        Err(SignatureError::UnknownKey(old.public().id()))
    );
}

#[test]
fn the_tests_signer_writes_what_minisign_reads() {
    // The tests' key signs like `minisign -S -l`: parse its output with
    // the same reader, and read the real file's layout back.
    let key = TestKey::trusted();
    let sig = key.sign(SUMS.as_bytes(), "rexen-center v0.2.0 SHA256SUMS");
    let file = SignatureFile::parse(&sig).unwrap();
    assert_eq!(file.key_id(), key.public().id());
    assert_eq!(file.trusted_comment(), "rexen-center v0.2.0 SHA256SUMS");
    assert_eq!(
        PublicKey::parse(&key.public().to_minisign()).unwrap(),
        key.public()
    );
    assert_eq!(minisign_key().to_minisign(), MINISIGN_KEY);
    let legacy = SignatureFile::parse(LEGACY).unwrap();
    assert_eq!(legacy.key_id(), "9894F660867DD46D");
}

#[test]
fn the_built_in_release_keys() {
    let keys = release_keys();
    assert!(!keys.is_empty());
    let mut ids: Vec<String> = keys.iter().map(PublicKey::id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), keys.len(), "key ids are unique");
    assert_eq!(keys[0].id(), "4B93885D9D72AAC9");
    // The tests' keys are never trusted by a real build.
    for seed in [TestKey::trusted(), TestKey::new(1), TestKey::new(2)] {
        assert!(!keys.contains(&seed.public()));
    }
    // Each built-in key has its `minisign.pub` file in packaging/, the same
    // key (the signing script and `minisign -V` use the file).
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/release-signing");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pub"))
        .collect();
    files.sort();
    assert_eq!(files.len(), RELEASE_KEYS.len(), "{files:?}");
    for (file, key) in files.iter().zip(RELEASE_KEYS) {
        let text = std::fs::read_to_string(file).unwrap();
        let mut lines = text.lines();
        let comment = lines.next().unwrap();
        let parsed = PublicKey::parse(key).unwrap();
        assert_eq!(
            comment,
            format!("untrusted comment: minisign public key {}", parsed.id())
        );
        assert_eq!(lines.next(), Some(*key), "{}", file.display());
    }
}

#[test]
fn base64() {
    for bytes in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
        let text = base64_encode(bytes);
        if !bytes.is_empty() {
            assert_eq!(base64_decode(&text).as_deref(), Some(bytes), "{text}");
        }
    }
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    for bad in ["", "Zm8", "Zm9=v", "Zm===", "Zm8!", "Zm9=", "Zm9vYm=="] {
        assert_eq!(base64_decode(bad), None, "{bad}");
    }
}
