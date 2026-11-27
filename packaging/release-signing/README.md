# Release signing

The app's updater installs a release only when the release's `SHA256SUMS`
is signed by a key it trusts. The signature is published beside it as
`SHA256SUMS.minisig`, in [minisign](https://jedisct1.github.io/minisign/)'s
format. The trusted public keys are built into the app
(`RELEASE_KEYS` in `crates/xen-update/src/signature.rs`), and each one's
`minisign.pub` file is in this folder. A test keeps the two the same.

| File | Key id | Created | Status |
|---|---|---|---|
| `rexen-release-1.pub` | `4B93885D9D72AAC9` | 2026-11-27 | current signing key |

```
untrusted comment: minisign public key 4B93885D9D72AAC9
RWTJqnKdXYiTS8U7nG6cjVGIH6sRYUZ09F+rVLOHHTzUmtAjTz3Oqc9k
```

The secret key is never in this repository. Whoever holds it can publish an
update that every installed copy will accept. Keep it offline, with a
backup, and add a password to it if you like (`minisign -C -s <key>`;
the signing script then asks for it).

## What is signed, and how

- The exact bytes of `SHA256SUMS`, which lists the SHA-256 of every package.
  The app checks the signature first and each download against that signed
  text.
- minisign's **legacy** mode (`minisign -S -l`): Ed25519 over the file
  itself, which the app checks with `ring`. Recent minisign versions sign a
  BLAKE2b hash of the file by default. The app refuses that mode and says to
  sign with `-l`. `packaging/sign-release.sh` always signs in legacy mode.
- The trusted comment is `rexen-center v<version> SHA256SUMS`. It is signed
  too, so `minisign -V` shows which release a signature was made for.

## Signing a release

Without the `RELEASE_SIGNING_KEY` repository secret, which is the default,
the release workflow creates the tag's release as a **draft** with the
packages and `SHA256SUMS`. The app never sees a draft. Then, on the machine
that holds the key:

```sh
gh release download v<version> --repo carcamano/rexen-center -D release-v<version>
packaging/sign-release.sh --key <path to the secret key> release-v<version>
cargo run -p xen-update --example verify-release -- release-v<version>
gh release upload v<version> release-v<version>/SHA256SUMS.minisig --repo carcamano/rexen-center
gh release edit v<version> --draft=false --repo carcamano/rexen-center
```

The script refuses to sign unless every file in the folder is listed in
`SHA256SUMS` exactly once and matches it, and every listed file is there. It
writes `SHA256SUMS.minisig` and checks it against the `.pub` files in this
folder. It never prints the key. Instead of `--key`, the key file's path can
be in `REXEN_SIGNING_KEY_FILE`, or its content in `REXEN_SIGNING_KEY`. The
`verify-release` example runs the app's own check with the built-in keys.
`minisign -V -p packaging/release-signing/rexen-release-1.pub -m SHA256SUMS`
does the same with the tool.

The folder can also be the workflow run's artifact
(`rexen-center-<version>`), signed before the release exists.

### Signing in the workflow instead

If the repository has the secret `RELEASE_SIGNING_KEY` (the whole content
of the minisign secret key file, without a password), the checksums job
signs `SHA256SUMS` with this script and the release is published directly.
This is quicker but weaker: anyone who can run code in the workflow can sign
with the key. That includes a compromised action or dependency, or someone
with write access to the workflow. The owner decides whether to set it. The
workflow passes the key only in the environment and never prints it.

## Rotating the key

The app trusts every key in `RELEASE_KEYS`, and a signature names its key
by id. To move to a new key:

1. Make the new key pair: `minisign -G -p rexen-release-<n>.pub -s
   rexen-release-<n>.key`. Keep the secret file offline.
2. Add `rexen-release-<n>.pub` to this folder and its second line to
   `RELEASE_KEYS`, keeping the old key. Update the table above.
3. Release that version signed with the **old** key. Installed copies trust
   the old key, accept the update, and from then on trust both.
4. Sign later releases with the new key. Once most users are on a version
   that has the new key, remove the old key from `RELEASE_KEYS` and its
   `.pub` file in a release signed with the new key.

Users who skip the transition release can still update with the old key for
as long as releases signed with it are published. After that they update by
hand once, from the release page.

**If a key is stolen:** stop using it at once. Do the rotation with a new
key, and remove the stolen key from `RELEASE_KEYS` in the same release
instead of keeping it. Copies that already have that release refuse
anything signed with the stolen key. Older copies still trust the stolen
key until they update, so say so in the release notes. If the secret was in
GitHub, delete the `RELEASE_SIGNING_KEY` secret too.

**If the key is lost:** installed copies cannot update automatically any
more. Make a new key, ship it in a release published unsigned, and tell
users to install that release by hand. Its dialog shows "not signed" and the
release page. From then on, sign with the new key.
