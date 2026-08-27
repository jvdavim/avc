# AVC

AVC, Artifact Version Control, tracks large files alongside Git without requiring Git LFS. Git stores small pointer files; AVC stores artifact bytes in a local content-addressed cache and can synchronize them with a remote object store.

Current release: `0.1.0` prototype.

## Status

Working local MVP:

- File-only artifact tracking
- SHA-256 content addressing
- Sibling pointer files, for example `model.bin.avc`
- Local cache with atomic writes
- Git worktree discovery
- Artifact status and integrity checks
- Safe checkout with dirty-file protection
- Local garbage collection
- Offline `file://` remotes for push and pull
- Remote artifact listing without downloading bytes

Cloud provider adapters are not implemented yet. S3, Google Cloud Storage, and Azure URLs can be configured, but transfers return an explicit unsupported-adapter error.

## Requirements

- Rust 1.75 or newer
- Git 2.30 or newer
- macOS, Linux, or Windows

## Build

Build debug binary:

```bash
cargo build
./target/debug/avc --help
```

Build optimized binary:

```bash
cargo build --release
./target/release/avc --version
```

Run without installing:

```bash
cargo run -p avc-cli -- --help
```

## Quick Start

Create or enter a Git worktree:

```bash
git init artifacts
cd artifacts
printf 'example artifact\n' > model.bin
```

Initialize AVC and track a file:

```bash
avc init
avc add model.bin
avc status
```

`add` creates `model.bin.avc`, stores bytes below `.avc/cache`, and adds the artifact path and cache paths to `.gitignore`. Commit pointer files and configuration with normal Git commands:

```bash
git add .avc/config.toml model.bin.avc .gitignore
git commit -m "Track model artifact"
```

## Commands

```text
avc init
avc remote add <name> <provider-url>
avc remote list
avc add <path> [<path>...]
avc list [--remote <name>]
avc status
avc commit <path> [<path>...] [--force]
avc push [<path>...] [--remote <name>]
avc pull [<path>...] [--remote <name>]
avc checkout [<path>...] [--force]
avc remove <path> [<path>...]
avc gc [--remote <name>] [--dry-run]
avc doctor
```

### Preview remote artifacts

`list` reads tracked pointer files and checks remote object availability. It does not download artifact bytes:

```bash
avc list --remote origin
```

Output:

```text
PATH    SIZE    OBJECT    REMOTE
model.bin       18      sha256:...    available
```

Artifact paths come from Git-visible pointer files. Remote object keys contain hashes only, so a remote cannot independently reconstruct user paths.

### Local remote test

Use a `file://` remote for offline development:

```bash
mkdir -p /tmp/avc-remote
avc remote add origin file:///tmp/avc-remote
avc list --remote origin
avc push --remote origin
avc list --remote origin
rm model.bin
avc pull --remote origin
```

`pull` downloads referenced objects into the cache, then materializes artifacts. It refuses to replace modified files unless `--force` is supplied to `checkout` directly.

## Pointer Files

Pointers use versioned YAML:

```yaml
version: 1
path: model.bin
object:
  algorithm: sha256
  hash: 7f...
  size: 4294967296
  media_type: application/octet-stream
```

Pointers contain repository-relative paths, exact byte size, and SHA-256 identity. See [`SPEC.md`](SPEC.md) for the compatibility and safety contract.

## Storage Layout

```text
.avc/
  config.toml
  config.local.toml
  cache/
    objects/sha256/<first-two-hash-characters>/<full-hash>
  state/
```

`.avc/config.toml` is shareable repository configuration. Credentials must not be stored there. Use provider-standard credential chains or ignored local configuration.

## Development

Run checks:

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The workspace currently contains:

- `avc-core`: domain types, pointer validation, hashing, and remote URL parsing
- `avc-cli`: command-line interface and local MVP workflows

Cloud adapters, Git revision selection, provider-neutral transfer contracts, and directory artifacts remain planned work. See [`PLAN.md`](PLAN.md).
