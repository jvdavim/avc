# Getting Started

This walkthrough takes you from an empty directory to an artifact pushed to a
remote and pulled back. It uses a local `file://` remote, so it needs no
credentials and no network — see [step 7](#7-configure-a-remote) for the S3
equivalent.

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

## 4. Track a directory

A directory is tracked the same way, as **one artifact with one pointer**:

```bash
mkdir -p data/raw
printf 'first\n'  > data/raw/one.csv
printf 'second\n' > data/raw/two.csv
avc add data/
```

```text
tracked data/ (2 file(s), 13 B, sha256:…)
```

Every file beneath `data/` is hashed and cached; a manifest naming them becomes
an object of its own, and `data.avc` points at it. Only `data.avc` goes into
Git — `data/` is added to `.gitignore`.

```bash
cat data.avc
```

```yaml
version: 1
path: data
kind: directory
object:
  algorithm: sha256
  hash: 0e80f9d32ad0dfdf6de8dce2230f3b7ce722720a3c3cf6ba3c4876a04c463456
  size: 266
  media_type: application/vnd.avc.tree+yaml
```

The directory's identity is that manifest's hash, so editing, adding, removing,
or renaming any file inside it makes the whole artifact `modified` — record the
new contents with `avc commit data`.

## 5. Check state

```bash
avc status
```

```text
ok      cached  model.bin
ok      cached  data/
```

The first column is the working-tree state (`ok`, `modified`, or `missing`); the
second is cache state (`cached` or `cache-missing`). A directory is `cached`
only when its manifest and every file it names are in the cache.

## 6. Commit to Git

Pointer files and shareable configuration belong in Git. The artifacts do not.

```bash
git add .avc/config.toml model.bin.avc data.avc .gitignore
git commit -m "Track model and data artifacts"
```

## 7. Configure a remote

For local development, use a `file://` remote — a plain directory that stands in
for an object store.

```bash
mkdir -p /tmp/avc-remote
avc remote add origin file:///tmp/avc-remote
```

Everything below works identically against S3 or an S3-compatible service. To
follow along with a local MinIO instead:

```bash
avc remote add origin s3+http://localhost:9000/my-bucket/artifacts
export AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin
```

For real S3, use `s3://my-bucket/artifacts` and your usual AWS environment
variables or `~/.aws/credentials`. See
[Configuration](configuration.md#credentials).

The first remote you add becomes the default. Confirm it:

```bash
avc remote list
```

```text
* origin File /tmp/avc-remote
```

## 8. Push and pull

Check what the remote has, without downloading anything:

```bash
avc list --remote origin
```

```text
PATH    SIZE    OBJECT  REMOTE
model.bin       17      sha256:1dfc4d…       missing
data/   13      sha256:0e80f9…       missing
```

For a directory, `SIZE` is the total bytes of the files it holds, and `REMOTE`
reads `available` only once the manifest *and* every file it names are there.

Upload the bytes:

```bash
avc push
```

Now simulate a fresh clone by deleting the artifact and restoring it:

```bash
rm -rf model.bin data
avc status          # missing  cached  model.bin
avc pull            # downloads to cache, then materializes the artifacts
avc status          # ok       cached  model.bin
find data -type f   # the whole directory is back
```

## 9. Update an artifact

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

The same applies to a directory. Change one file in it and only that file
becomes a new object; the rest are already stored:

```bash
printf 'third\n' > data/raw/three.csv
avc status          # modified  cached  data/
avc commit data
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

## 10. Verify integrity

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
