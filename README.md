# AVC

<p align="center">
  <a href="https://github.com/jvdavim/avc/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/jvdavim/avc/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/jvdavim/avc/blob/main/LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="Rust 1.75+" src="https://img.shields.io/badge/rust-1.75%2B-orange.svg">
  <img alt="Status: prototype" src="https://img.shields.io/badge/status-prototype-yellow.svg">
</p>

**Artifact Version Control — version large files alongside Git, without Git LFS.**

Git stores small YAML pointer files. AVC stores the bytes in a local
content-addressed cache and synchronizes them with an object store. Your model
weights, datasets, and archives stay out of Git history; their exact identity
stays in it.

```bash
avc add model.bin      # hash it, cache it, write model.bin.avc
git add model.bin.avc  # commit the pointer, not the 4 GB
avc push               # send the bytes to your object store
```

## Highlights

- 📦 **Keeps Git repositories small.** A 4 GB checkpoint committed ten times is a
  40 GB clone, forever. AVC keeps the bytes out of history entirely.
- 🔒 **Content-addressed and verified.** SHA-256 over exact bytes. Every cache
  read checks size *and* digest, so silent corruption is detected, not served.
- 🪶 **Nothing special required from your Git server.** No LFS batch API, no
  server hooks, no `git lfs install` on every clone. Just the binary.
- 🧠 **Bounded memory.** Hashing streams in 64 KiB chunks. A 100 GB artifact
  costs the same memory as a 100 byte one.
- 🛡️ **Refuses to lose your data.** Atomic writes everywhere. Modified files are
  never overwritten without `--force`. No command deletes remote data.
- 🕵️ **No path leakage.** Object keys contain hashes only — a shared bucket never
  learns your repository's directory structure.
- 🎯 **Explicit providers.** Transport is chosen by URL scheme, never guessed
  from a hostname.
- 🧩 **Deduplicates by construction.** Identical bytes have one key, whether they
  appear in ten paths or ten branches.
- 🦀 **Small dependency tree.** Nine direct dependencies, no async runtime. S3
  is spoken over plain HTTP with a hand-written SigV4 signer rather than a
  cloud SDK. A tool guarding your artifacts should not be a supply-chain
  liability.

> [!IMPORTANT]
> AVC is a **`0.1.0` prototype**. The local workflow and S3 transport work end
> to end; `gs://` and `az://` still configure correctly and then return an
> explicit unsupported-adapter error on transfer. On-disk formats are
> provisional. See the [roadmap](docs/roadmap.md).

## Installation

AVC is not yet on crates.io. Install from source with Rust 1.75 or newer:

```bash
git clone https://github.com/jvdavim/avc.git
cd avc
cargo install --path crates/avc-cli
```

Or build the binary directly:

```bash
cargo build --release
./target/release/avc --version
```

Requirements: Rust 1.75+, Git 2.30+, on macOS, Linux, or Windows.

## Documentation

Full documentation lives in [`docs/`](docs/README.md).

- [Getting Started](docs/getting-started.md) — install to first push, end to end
- [Concepts](docs/concepts.md) — pointers, objects, cache, remotes
- [CLI Reference](docs/cli.md) — every command, flag, and exit code
- [Configuration](docs/configuration.md) — remote URLs and credentials
- [Architecture](docs/architecture.md) — how the crates fit together
- [Roadmap](docs/roadmap.md) — what is built, what is not, what is next
- [`SPEC.md`](SPEC.md) — the normative format and safety contract

## Getting started

Track your first artifact in a Git repository:

```bash
git init artifacts && cd artifacts
printf 'example artifact\n' > model.bin

avc init
avc add model.bin
avc status
```

```text
ok      cached  model.bin
```

`add` hashes the file, stores the bytes under `.avc/cache`, writes a pointer at
`model.bin.avc`, and adds `model.bin` to `.gitignore`. Commit the pointer with
ordinary Git:

```bash
git add .avc/config.toml model.bin.avc .gitignore
git commit -m "Track model artifact"
```

Configure a remote and push the bytes:

```bash
mkdir -p /tmp/avc-remote
avc remote add origin file:///tmp/avc-remote
avc push
```

Or push to S3, or anything that speaks the S3 API:

```bash
avc remote add origin s3://my-bucket/artifacts               # Amazon S3
avc remote add minio s3+http://localhost:9000/my-bucket      # MinIO, R2, Ceph…
export AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=…
avc push
```

Then simulate a fresh clone:

```bash
rm model.bin
avc pull       # downloads into the cache, then materializes the file
avc doctor     # re-hashes everything and confirms integrity
```

See [Getting Started](docs/getting-started.md) for the full walkthrough.

## Features

### Tracking

`avc add` starts tracking a file; `avc commit` records a new version of one
already tracked. Both stream the file through SHA-256, deduplicate against the
cache, and write a canonical pointer. `avc remove` stops tracking without
deleting anything.

### Pointers

A pointer is versioned YAML with fixed field order, LF endings, and no
timestamps — so the same artifact always produces byte-identical output, and Git
diffs stay meaningful:

```yaml
version: 1
path: model.bin
object:
  algorithm: sha256
  hash: 1dfc4d103921b3462e1c482b3019f6e1838ec62eb9dbd67ffe4602325dd82fe2
  size: 17
  media_type: null
```

Validation is strict — unknown fields are rejected, and `path` may not escape the
repository root. A pointer drives filesystem writes, so it is treated as
untrusted input.

### Inspection

`avc status` reports working-tree state (`ok`, `modified`, `missing`) and cache
state (`cached`, `cache-missing`) per artifact. `avc list --remote origin` shows
what a remote holds **without downloading bytes**. `avc doctor` re-hashes cached
objects and fails on any drift.

### Transfers

`avc push` and `avc pull` move objects between the cache and a remote, optionally
scoped to specific paths. `avc pull` materializes files afterward, refusing to
clobber local modifications. `avc checkout` restores from cache without touching
the network.

Transfers stream in bounded memory in both directions — pushing a 300 MB
artifact peaks under 5 MB of RSS. A download is hashed as it is written, so a
truncated or tampered object is rejected before it can enter the cache. `avc
push` asks the remote what it already has, making a repeated push a no-op
rather than a re-upload.

### S3 and S3-compatible storage

`s3://` reaches Amazon S3; `s3+https://` and `s3+http://` reach anything else
that speaks the S3 API — MinIO, Cloudflare R2, Ceph, Backblaze B2 — with the
host taken as the endpoint and the first path segment as the bucket. Requests
are signed with SigV4 and sent over plain HTTP; there is no cloud SDK and no
async runtime.

Credentials resolve in this order, so AVC never becomes another place a secret
leaks from:

1. `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`
2. `.avc/config.local.toml`, which `avc init` gitignores
3. `~/.aws/credentials` for `$AWS_PROFILE`, or `default`

See [Configuration](docs/configuration.md#credentials) for the full table.

### Storage

```text
.avc/
  config.toml                                   # tracked: remotes, buckets, prefixes
  config.local.toml                             # ignored: local overrides
  cache/objects/sha256/<first-two>/<full-hash>
  state/
```

Remotes mirror this layout under an optional prefix. `avc gc` reclaims cache
objects no pointer references — read the [caveat](docs/cli.md#avc-gc) first.

## Platform support

Linux, macOS, and Windows, tested on each in CI. Atomic-rename semantics differ
across platforms and are handled explicitly.

## Versioning

AVC is `0.x`. While the format is provisional, a minor release may include a
breaking on-disk change; each is documented in
[`CHANGELOG.md`](CHANGELOG.md). The conditions for freezing `version: 1` are
listed in the [roadmap](docs/roadmap.md#format-freeze).

## Contributing

Contributions are welcome, and you do not need to write Rust to help — bug
reports, documentation fixes, and reports of friction are all genuinely useful on
a project this young.

Start with [`CONTRIBUTING.md`](CONTRIBUTING.md), the full
[contributing guide](docs/contributing.md), and the
[development setup](docs/development.md). Items marked **good first issue** in the
[roadmap](docs/roadmap.md) are scoped to be approachable.

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md). To report
a vulnerability, see [`SECURITY.md`](SECURITY.md) — please do not open a public
issue.

## Acknowledgements

AVC's pointer-file approach follows the path cut by [Git LFS] and
[DVC], and its content-addressed store is the model Git itself
established. The `.avc` sibling-pointer design and the decision to keep
materialization explicit are departures from both, taken so that AVC needs
nothing from the Git server or from a contributor's Git installation.

[Git LFS]: https://git-lfs.com
[DVC]: https://dvc.org

## License

AVC is licensed under the MIT License. See [`LICENSE`](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in AVC by you shall be licensed as above, without any additional
terms or conditions.
