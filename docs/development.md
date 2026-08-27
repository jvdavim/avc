# Development

Setting up an environment, running the checks, and debugging AVC locally.

For contribution process — issues, pull requests, review — see
[Contributing](contributing.md).

## Prerequisites

| Tool | Minimum | Notes |
| --- | --- | --- |
| Rust | 1.75 | Workspace MSRV. CI verifies builds on it. |
| Git | 2.30 | Required at runtime; AVC discovers the worktree root |

Install Rust via [rustup](https://rustup.rs):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add rustfmt clippy
```

## Clone and build

```bash
git clone https://github.com/jvdavim/avc.git
cd avc
cargo build
./target/debug/avc --help
```

Release build:

```bash
cargo build --release
./target/release/avc --version
```

Run without installing:

```bash
cargo run -p avc-cli -- status
```

Note the `--`: everything after it goes to `avc`, not to Cargo.

## The checks

CI runs exactly these four. Run them before pushing.

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

To fix formatting rather than just check it:

```bash
cargo fmt --all
```

A single command for the whole set:

```bash
cargo fmt --all -- --check && \
  cargo check --workspace && \
  cargo test --workspace && \
  cargo clippy --workspace --all-targets -- -D warnings
```

## Verifying the MSRV

The workspace declares `rust-version = "1.75"`. A change that needs a newer
feature will fail on the MSRV job in CI. To check locally:

```bash
rustup toolchain install 1.75
cargo +1.75 check --workspace
```

Raising the MSRV is a deliberate decision — open an issue rather than bumping it
to satisfy a dependency.

## Tests

Five unit tests live at the bottom of `crates/avc-core/src/lib.rs`:

```bash
cargo test --workspace
```

```text
running 5 tests
test tests::parses_explicit_remote_schemes ... ok
test tests::supports_unicode_repository_paths ... ok
test tests::rejects_invalid_pointer_data ... ok
test tests::pointer_serialization_is_stable_and_round_trips ... ok
test tests::hashes_stream_without_loading_all_bytes ... ok
```

Run one test, with output:

```bash
cargo test --workspace pointer_serialization -- --nocapture
```

`tests/object_store.rs` holds one `ObjectStore` contract that both backends must
satisfy. The `file://` half runs on every CI run; the S3 half is `#[ignore]`d
until you point it at a real server:

```bash
export AVC_TEST_S3_ENDPOINT=http://127.0.0.1:9000
export AVC_TEST_S3_BUCKET=avc-test
export AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin
cargo test -p avc-core --test object_store -- --ignored
```

Any new backend should be added to that suite rather than given its own, so all
transports are held to one standard.

There is **no CLI integration test harness yet**. Building one is on the
[Roadmap](roadmap.md) and is a high-value, low-prerequisite contribution.

## Testing against MinIO

```bash
docker run -d --rm -p 9000:9000 -e MINIO_ROOT_USER=minioadmin \
  -e MINIO_ROOT_PASSWORD=minioadmin minio/minio server /data
```

Create a bucket (the MinIO console on `:9001`, or `mc mb local/avc-test`), then
point a scratch repository at it as shown below. MinIO is the reference target
for the S3 adapter: it exercises path-style addressing, a non-default port, and
plain HTTP, which is the combination most likely to break a signer.

## A scratch repository for manual testing

AVC only operates inside a Git worktree, so manual testing needs one. This script
builds a throwaway repository with a `file://` remote:

```bash
#!/usr/bin/env bash
set -euo pipefail

AVC="$(pwd)/target/debug/avc"
WORK="$(mktemp -d)"
REMOTE="$(mktemp -d)"

git init -q "$WORK/repo"
cd "$WORK/repo"

printf 'example artifact\n' > model.bin

"$AVC" init
"$AVC" add model.bin
"$AVC" remote add origin "file://$REMOTE"
# Or, against a local MinIO:
#   "$AVC" remote add origin "s3+http://localhost:9000/avc-test/artifacts"
#   export AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin
"$AVC" status
"$AVC" push
"$AVC" list --remote origin

rm model.bin
"$AVC" pull
"$AVC" status
"$AVC" doctor

echo "scratch repo: $WORK/repo"
echo "scratch remote: $REMOTE"
```

Save it as `scratch.sh`, `chmod +x scratch.sh`, and run it from the workspace
root after `cargo build`.

## Inspecting state by hand

Everything AVC writes is plain text or content-addressed files, so debugging
rarely needs a debugger.

```bash
# A pointer file
cat model.bin.avc

# Repository configuration
cat .avc/config.toml

# Every cached object
find .avc/cache/objects -type f

# Verify an object independently of AVC
sha256sum .avc/cache/objects/sha256/1d/1dfc4d10*
```

The last one is worth internalizing: an object's filename *is* its SHA-256, so
`sha256sum` gives you an independent check on whether AVC did the right thing.

## Debug output

There is no logging framework and no `--verbose` yet. For ad-hoc tracing use
`eprintln!` and remove it before pushing. Adding structured logging behind a flag
would be a welcome contribution.

For backtraces on a panic:

```bash
RUST_BACKTRACE=1 cargo run -p avc-cli -- status
```

## Working on `avc-core`

`avc-core` has no filesystem dependencies beyond reading bytes to hash, so most
changes can be driven entirely from unit tests — no scratch repository needed.
That is the fastest loop in the project. Prefer putting logic there when the
choice exists.

## Working on `avc-cli`

`main.rs` holds every workflow. When adding a command:

1. Add a variant to `Command`, plus an `Args` struct if it takes flags.
2. Add the match arm in `run`.
3. Write the command function returning `Result<(), String>`.
4. Reuse the existing helpers — `load_repo`, `pointer_files`, `cache_path`,
   `choose_remote`, `write_atomic`, `copy_atomic`.
5. Document it in [CLI Reference](cli.md).

**Any new write to the cache or worktree must go through `write_atomic` or
`copy_atomic`.** Direct `fs::write` breaks the atomicity guarantee in `SPEC.md`.

## Common issues

**`not inside a Git worktree`** — you are outside a repository. AVC walks up from
the current directory looking for `.git`.

**`AVC is not initialized; run 'avc init'`** — `.avc/config.toml` is missing.

**`cache object missing for <path>`** — the pointer references an object not in
the cache. Run `avc pull`, or re-`add` the file if you have it locally.

**`refusing to replace modified file <path>; use --force`** — working copy
differs from the pointer. This is the safety check working; confirm you want to
discard local changes before forcing.

**Clippy fails in CI but not locally** — your toolchain is older than CI's.
`rustup update`.

## Editor setup

`rust-analyzer` works with no configuration. For VS Code, the `rust-analyzer`
extension plus:

```json
{
  "editor.formatOnSave": true,
  "rust-analyzer.check.command": "clippy"
}
```

`.gitignore` already covers `.idea/` and `.vscode/`.

## Release process

AVC is not yet published to crates.io. When it is, the outline is:

1. Update `version` in `[workspace.package]`.
2. Update `CHANGELOG.md`.
3. Run the full check set.
4. Tag `vX.Y.Z` and push the tag.
5. `cargo publish -p avc-core` then `cargo publish -p avc-cli` — order matters,
   `avc-cli` depends on `avc-core`.

Until the format is frozen, releases stay `0.x` and may include breaking format
changes. See [Roadmap](roadmap.md#format-freeze).
