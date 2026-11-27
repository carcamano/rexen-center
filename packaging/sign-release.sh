#!/usr/bin/env bash
# Signs a release's SHA256SUMS with the release-signing key, for the app's
# updater (crates/xen-update/src/signature.rs), and writes
# SHA256SUMS.minisig beside it, to upload to the release.
#
#   packaging/sign-release.sh [--key FILE] [--version X.Y.Z] [--force] DIR
#
# DIR holds the release's files as published or as the release workflow
# built them: SHA256SUMS and every package (`gh release download v<version>
# -D DIR`, or the workflow run's artifact). Before signing, every file in DIR
# must be listed in SHA256SUMS exactly once and match it, and every file it
# lists must be there: what is signed is what is published.
#
# The key (minisign's secret key file) is given by --key, or by
# REXEN_SIGNING_KEY_FILE (its path), or by REXEN_SIGNING_KEY (its content, as
# the release workflow passes a repository secret; written to a private
# temporary file and removed). The key is never printed. A key with a
# password asks for it.
#
# The signature is minisign's legacy mode (`-l`: Ed25519 over the file
# itself), the one the updater accepts; it is checked afterwards against the
# public keys in packaging/release-signing/, the ones built into the app.
#
# Needs minisign (macOS: `brew install minisign`; Debian/Ubuntu: `apt install
# minisign`). See packaging/release-signing/README.md.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
keys_dir="$here/release-signing"

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
    exit 2
}

die() {
    echo "sign-release: $*" >&2
    exit 1
}

key_file=${REXEN_SIGNING_KEY_FILE:-}
version=
force=
dir=
while [ $# -gt 0 ]; do
    case $1 in
    --key)
        [ $# -ge 2 ] || usage
        key_file=$2
        shift 2
        ;;
    --version)
        [ $# -ge 2 ] || usage
        version=$2
        shift 2
        ;;
    --force)
        force=1
        shift
        ;;
    -h | --help) usage ;;
    -*) usage ;;
    *)
        [ -z "$dir" ] || usage
        dir=$1
        shift
        ;;
    esac
done
[ -n "$dir" ] || usage
[ -d "$dir" ] || die "$dir is not a folder"
command -v minisign >/dev/null || die "minisign is not installed (brew install minisign / apt install minisign)"

sums="$dir/SHA256SUMS"
sig="$dir/SHA256SUMS.minisig"
[ -f "$sums" ] || die "no SHA256SUMS in $dir"
if [ -e "$sig" ] && [ -z "$force" ]; then
    die "$sig exists already (--force to sign again)"
fi

# The key: a file, or the content of REXEN_SIGNING_KEY in a private file.
tmp_key=
cleanup() {
    if [ -n "$tmp_key" ]; then
        rm -f "$tmp_key"
    fi
}
trap cleanup EXIT
if [ -z "$key_file" ] && [ -n "${REXEN_SIGNING_KEY:-}" ]; then
    tmp_key=$(umask 077 && mktemp "${TMPDIR:-/tmp}/rexen-signing-key.XXXXXX")
    printf '%s\n' "$REXEN_SIGNING_KEY" >"$tmp_key"
    key_file=$tmp_key
fi
[ -n "$key_file" ] || die "no key: give --key FILE, REXEN_SIGNING_KEY_FILE or REXEN_SIGNING_KEY"
[ -f "$key_file" ] || die "the key file $key_file does not exist"

# 1. SHA256SUMS against the folder: well formed, each file once, each
#    file listed, and each digest right.
if command -v sha256sum >/dev/null; then
    sha256=(sha256sum)
else
    sha256=(shasum -a 256)
fi
listed=$(sed -n 's/^[0-9a-fA-F]\{64\} [ *]\(.*\)$/\1/p' "$sums")
lines=$(grep -c . "$sums" || true)
[ "$lines" -gt 0 ] || die "SHA256SUMS is empty"
[ "$(printf '%s\n' "$listed" | grep -c .)" -eq "$lines" ] || die "SHA256SUMS has malformed lines"
dupes=$(printf '%s\n' "$listed" | sort | uniq -d)
[ -z "$dupes" ] || die "SHA256SUMS lists more than once: $dupes"
while IFS= read -r name; do
    [ -f "$dir/$name" ] || die "SHA256SUMS lists $name, which is not in $dir"
done <<<"$listed"
# Hidden files are not uploaded (`gh release create v… dir/*`): not checked.
for path in "$dir"/*; do
    [ -e "$path" ] || continue
    name=$(basename "$path")
    case $name in SHA256SUMS | SHA256SUMS.minisig) continue ;; esac
    [ -f "$path" ] || die "$name in $dir is not a file"
    printf '%s\n' "$listed" | grep -qxF -- "$name" || die "$name is in $dir but not in SHA256SUMS"
done
(cd "$dir" && "${sha256[@]}" --check --strict --quiet SHA256SUMS) ||
    die "the files in $dir do not match SHA256SUMS"
count=$(printf '%s\n' "$listed" | grep -c .)
echo "SHA256SUMS lists the $count files in $dir, and they match it."

# 2. The version, for the signature's trusted comment.
if [ -z "$version" ]; then
    version=$(printf '%s\n' "$listed" | sed -n 's/^rexen-center-\([0-9][^-]*\)-linux-x86_64\.tar\.gz$/\1/p' | head -n 1)
fi
[ -n "$version" ] || die "cannot tell the version from the file names; give --version"

# 3. Sign (minisign's legacy mode), then check against the app's keys.
rm -f "$sig"
minisign -S -l -s "$key_file" -m "$sums" -x "$sig" \
    -c "signature from the ReXen Center release key" \
    -t "rexen-center v$version SHA256SUMS" >/dev/null ||
    die "minisign could not sign"
cleanup
tmp_key=
trusted=
for pub in "$keys_dir"/*.pub; do
    [ -f "$pub" ] || continue
    if minisign -V -q -p "$pub" -m "$sums" -x "$sig" >/dev/null 2>&1; then
        trusted=$pub
        break
    fi
done
if [ -z "$trusted" ]; then
    rm -f "$sig"
    die "the key that signed is not one of the app's keys in $keys_dir; nothing written"
fi
echo "Signed: $sig"
echo "  key $(sed -n 's/^untrusted comment: minisign public key //p' "$trusted") ($(basename "$trusted")), trusted comment: rexen-center v$version SHA256SUMS"
echo "Upload it to the release, then publish the release if it is a draft:"
echo "  gh release upload v$version '$sig' --repo carcamano/rexen-center"
echo "  gh release edit v$version --draft=false --repo carcamano/rexen-center"
