# Getting Started

This walkthrough takes you from an empty directory to an artifact pushed to a
remote and pulled back. It uses a local `file://` remote, which is the only
transport implemented in `0.1.0`.

## Requirements

| Tool | Minimum | Why |
| --- | --- | --- |
| Rust | 1.75 | Workspace MSRV, declared in `Cargo.toml` |
| Git | 2.30 | AVC discovers the repository root by walking up to `.git` |
| OS | macOS, Linux, or Windows | Atomic rename semantics differ but are handled |

AVC is not yet published to crates.io, so install from source.

## Install

```bash
git clone https://github.com/jvdavim/avc.git
cd avc
cargo build --release
```

The binary lands at `target/release/avc`. Put it on your `PATH`:

```bash
install -m 755 target/release/avc ~/.local/bin/avc
avc --version
```

Alternatively, install straight from the checkout with Cargo:

```bash
cargo install --path crates/avc-cli
```

To try AVC without installing anything:

```bash
cargo run -p avc-cli -- --help
```

## 1. Create a repository

AVC requires a Git worktree. It refuses to initialize anywhere else.

```bash
git init artifacts
cd artifacts
printf 'example artifact\n' > model.bin
```

## 2. Initialize AVC

```bash
avc init
```

This creates `.avc/cache/`, `.avc/state/`, and `.avc/config.toml`, then appends
`.avc/cache/` and `.avc/config.local.toml` to `.gitignore`. It prints:

```text
initialized AVC in /path/to/artifacts
```

## 3. Track an artifact

```bash
avc add model.bin
```

Three things happen:

- `model.bin` is streamed through SHA-256 and copied into
  `.avc/cache/objects/sha256/<first-two>/<full-hash>`.
- A pointer file `model.bin.avc` is written.
- `model.bin` is appended to `.gitignore`, so Git tracks the pointer, not the
  bytes.

Inspect the pointer:

```bash
cat model.bin.avc
```

```yaml
version: 1
path: model.bin
object:
  algorithm: sha256
  hash: 1dfc4d103921b3462e1c482b3019f6e1838ec62eb9dbd67ffe4602325dd82fe2
  size: 17
  media_type: null
```

`media_type` is optional metadata; `avc add` does not infer it, so it is written
as `null`.

## 4. Check state

```bash
avc status
```

```text
ok      cached  model.bin
```

The first column is the working-tree state (`ok`, `modified`, or `missing`); the
second is cache state (`cached` or `cache-missing`).

## 5. Commit to Git

Pointer files and shareable configuration belong in Git. The artifact does not.

```bash
git add .avc/config.toml model.bin.avc .gitignore
git commit -m "Track model artifact"
```

## 6. Configure a remote

For local development, use a `file://` remote — a plain directory that stands in
for an object store.

```bash
mkdir -p /tmp/avc-remote
avc remote add origin file:///tmp/avc-remote
```

The first remote you add becomes the default. Confirm it:

```bash
avc remote list
```

```text
* origin File /tmp/avc-remote
```

## 7. Push and pull

Check what the remote has, without downloading anything:

```bash
avc list --remote origin
```

```text
PATH    SIZE    OBJECT  REMOTE
model.bin       17      sha256:1dfc4d…       missing
```

Upload the bytes:

```bash
avc push
```

Now simulate a fresh clone by deleting the artifact and restoring it:

```bash
rm model.bin
avc status          # missing  cached  model.bin
avc pull            # downloads to cache, then materializes the file
avc status          # ok       cached  model.bin
```

## 8. Update an artifact

When the file changes, `status` reports it:

```bash
printf 'updated artifact\n' > model.bin
avc status          # modified  cached  model.bin
```

Record the new version with `commit`, which — unlike `add` — requires that a
pointer already exists:

```bash
avc commit model.bin
git add model.bin.avc && git commit -m "Update model"
avc push
```

The old object stays in the cache so older commits remain checkoutable. Reclaim
space once you no longer need it:

```bash
avc gc --dry-run    # show what would go
avc gc              # delete objects no pointer in the worktree references
```

> **Careful:** `gc` reachability is computed only from pointer files present in
> the *current working tree*. Objects referenced solely by other branches or by
> historical commits are considered unreachable. See
> [CLI Reference](cli.md#avc-gc).

## 9. Verify integrity

```bash
avc doctor
```

```text
doctor: repository, pointers, and available cache objects are valid
```

`doctor` re-hashes every cached object and fails if any byte drifted from what
its pointer claims.

## Cloning a repository that already uses AVC

```bash
git clone https://github.com/example/project.git
cd project
avc remote add origin file:///tmp/avc-remote   # if not already in config.toml
avc pull
```

`.avc/config.toml` is committed, so remotes are usually already configured and
only credentials are local.

## Next steps

- [Concepts](concepts.md) — what pointers, objects, and the cache actually are
- [CLI Reference](cli.md) — every command and flag
- [Configuration](configuration.md) — remote URLs and credential handling
