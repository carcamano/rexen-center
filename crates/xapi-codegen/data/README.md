# Pinned XenAPI datamodel

`cargo run -p xapi-codegen` reads these files and writes
`crates/xapi/src/generated/`. Both data files are checked against
`SHA256SUMS` on every run, and the test `generated_code_is_up_to_date` fails
if the committed bindings differ from what the generator produces.

| File | What it is |
|---|---|
| `xenapi.json` | Output of xen-api's `gen_json` (`ocaml/idl/json_backend`) at tag **v25.27.0** (commit `c7592d75cd92b21f9b618840ba814d15f2fa4325`): 70 classes, every field, message, enum and lifecycle. |
| `async-messages.json` | For the same API, the messages that have an `Async.*` variant (`msg_async`). `gen_json` does not export this flag, so `ocaml/gen_async.ml` reads it from the same datamodel. 403 messages. |
| `SHA256SUMS` | Pinned hashes of both files. |
| `ocaml/` | The sidecar generator (`gen_async.ml`, `dune`) and the macOS build patch for `gen_json` (`gen_json-macos-io.patch`). |

## Why v25.27.0

XCP-ng 8.3 is the newest release this client targets (API 2.21). The lab
host it was validated against reports `software_version.xapi_build =
25.27.0`, so the datamodel matches that server exactly. Older servers
(XCP-ng 8.2, API 2.16) work too: the client ignores fields it does not
receive, keeps enum values it does not know (`Unknown(String)`) and treats a
class the server does not have as empty.

## Provenance check

The `xenapi.json` built locally is byte-identical to the copy the xen-api CI
published for the same commit: `_data/xenapi.json` in
`xapi-project/xapi-project.github.io` at commit
`cb749817c05ff37b1460916882403c9e87320ab9` ("deploy:
xapi-project/xen-api@c7592d75…").

sha256 `1b966c5e5a394c188aa544330da8f9d10158039aa991af33b367feb4d56cc9fa`.

## Regenerating

Needs `opam` (for example `brew install opam`); no Docker. On macOS the
`xapi-stdext-unix` library needs the Linux-only `polly` binding, but
`gen_json` uses it only for two file helpers, so `gen_json-macos-io.patch`
replaces those with the OCaml standard library. The datamodel and the
emitted JSON are unchanged; the byte-identical match above confirms it.

```sh
export OPAMROOT=$PWD/opam-root OPAMYES=1
opam init --bare -n --disable-sandboxing
opam switch create xapi ocaml-base-compiler.4.14.2
eval "$(opam env --switch=xapi --set-switch)"
opam install dune rpclib ppx_deriving_rpc sexplib0 sexplib yojson fmt astring \
  xapi-backtrace ppx_sexp_conv ptime mtime uuidm re rresult logs menhir unix-errno

git clone --depth 1 --branch v25.27.0 https://github.com/xapi-project/xen-api.git
cd xen-api
git apply <this dir>/ocaml/gen_json-macos-io.patch
mkdir -p ocaml/idl/json_backend/async_sidecar
cp <this dir>/ocaml/gen_async.ml <this dir>/ocaml/dune ocaml/idl/json_backend/async_sidecar/
dune build ocaml/idl/json_backend/gen_json.exe ocaml/idl/json_backend/async_sidecar/gen_async.exe
./_build/default/ocaml/idl/json_backend/gen_json.exe -destdir out   # writes out/_data/xenapi.json
./_build/default/ocaml/idl/json_backend/async_sidecar/gen_async.exe out/async-messages.json
```

Then copy `out/_data/xenapi.json` and `out/async-messages.json` here, update
`SHA256SUMS` (`shasum -a 256 xenapi.json async-messages.json > SHA256SUMS`)
and `DATAMODEL_SOURCE` in `src/lib.rs`, and run `cargo run -p xapi-codegen`.

## License

The datamodel comes from xen-api (LGPL-2.1 with the OCaml linking
exception). Only the data is used here, as input to our own generator.
