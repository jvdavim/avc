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
avc add data/          # or a whole directory, as one artifact
git add model.bin.avc  # commit the pointer, not the 4 GB
avc push               # send the bytes to your object store
```

In CI, name the repository and the path you need — never a bucket. What you name
lands where you asked for it, under its own name:

```bash
avc fetch --repo https://github.com/acme/artifacts models/bert -o .   # ./bert/…
avc fetch --repo https://github.com/acme/artifacts data/raw/2024.csv -o .  # ./2024.csv
```

## Highlights

- 📦 **Keeps Git repositories small.** A 4 GB checkpoint committed ten times is a
  40 GB clone, forever. AVC keeps the bytes out of history entirely.
- 🔒 **Content-addressed and verified.** SHA-256 over exact bytes. Every cache
  read checks size *and* digest, so silent corruption is detected, not served.
- 🪶 **Nothing special required from your Git server.** No LFS batch API, no
  server hooks, no `git lfs install` on every clone. Just the binary.
- 🧠 **Bounded memory.** Hashing streams in fixed-size chunks. A 100 GB artifact
  costs the same memory as a 100 byte one, and `avc add` hashes and stores in a
  single pass over the bytes rather than reading the file twice.
- 🛡️ **Refuses to lose your data.** Atomic writes everywhere. Modified files are
  never overwritten without `--force`. No command deletes remote data.
- 🕵️ **No path leakage.** Object keys contain hashes only — a shared bucket never
  learns your repository's directory structure.
- 🎯 **Explicit providers.** Transport is chosen by URL scheme, never guessed
  from a hostname.
- 🏢 **Works behind a corporate proxy.** A TLS-inspecting network is a supported
  configuration, not a wall: trust the machine's own certificate store or name a
  PEM bundle. Verification can never be switched off.
- 🧩 **Deduplicates by construction.** Identical bytes have one key, whether they
  appear in ten paths, ten branches, or twice inside one tracked directory.
- 📁 **Directories are one artifact.** `avc add data/` tracks a whole tree behind
  a single pointer; editing one file in a thousand re-versions the directory
  without re-storing the other 999. Every file in it is still an object of its
  own, so a consumer can fetch just one of them.
- 🏗️ **An artifact registry, not just a repository.** One repository holds many
  projects' artifacts; `avc fetch --repo <git-url> models/bert` takes just that
  path, with no clone, no `avc init`, and no cache. The object store is read
  from the repository's own config, so consumers never name a bucket.
- 🎁 **Delivers what you asked for.** A fetched path arrives in your output
  directory under its own name — no `models/` recreated around it — and two
  paths that would collide are refused rather than one overwriting the other.
- 🚚 **Migrates from DVC without moving the bytes.** `avc migrate dvc` replays
  a DVC project's whole history — every branch, tag, and merge — with its `.dvc`
  files rewritten as pointers. Migrated objects keep the MD5 identity DVC gave
  them, so pointing the migration at the DVC remote's own bucket makes it a
  server-side copy: no artifact bytes cross the network. Interrupted runs
  resume.
- 🦀 **Small dependency tree.** Ten direct dependencies, no async runtime. S3
  is spoken over plain HTTP with a hand-written SigV4 signer rather than a
  cloud SDK. A tool guarding your artifacts should not be a supply-chain
  liability.

> [!IMPORTANT]
> AVC is a **`0.1.0` prototype**. The local workflow and S3 transport work end
> to end; `gs://` and `az://` still configure correctly and then return an
> explicit unsupported-adapter error on transfer. On-disk formats are
> provisional. See the [roadmap](docs/roadmap.md).

> [!WARNING]
> **AVC is a vibe-coded project.** Nearly all of its code, tests, and
> documentation were written by AI coding assistants, directed and reviewed by a
> human maintainer. That is worth knowing before you trust it with anything:
>
> - **Read it before you rely on it.** The test suite is real and passes, but a
>   generated codebase can be confidently wrong in ways review does not always
>   catch, and prose that reads authoritatively is not evidence that the code
>   beneath it was exercised.
> - **The documentation is generated too.** Where these documents and the code
>   disagree, the code is what runs. Report the mismatch — that is a bug.
> - **Nothing here has been audited**, by a security reviewer or otherwise.
> - **It has not been run at scale in production** by anyone.
>
> The design decisions are deliberate and the tests are written to be
> falsifiable rather than decorative, but treat this as what it is: a young
> prototype whose confidence exceeds its mileage. Back up anything you cannot
> lose, and see [`SECURITY.md`](SECURITY.md).

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
- [CI/CD](docs/ci-cd.md) — fetching artifacts in a pipeline
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
STATUS  CACHE   SIZE  ARTIFACT
ok      cached  17 B  model.bin

1 artifact: 1 ok, 0 modified, 0 missing
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

Everything after the bucket is the key prefix, so one bucket serves many
repositories. The bucket's region and the AWS profile to authenticate with can
be recorded alongside it — names, not secrets, so they are safe to commit:

```bash
avc remote add origin s3://my-bucket/team-a/artifacts --region sa-east-1 --profile artifacts
avc push       # signs for sa-east-1 with the [artifacts] profile from ~/.aws
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

`avc add` starts tracking a file or a directory; `avc commit` records a new
version of one already tracked. Both stream through SHA-256, deduplicate against
the cache, and write a canonical pointer. `avc remove` stops tracking without
deleting anything.

A directory is one artifact with one pointer:

```bash
avc add data/
```

```text
tracked      data/ (3 files, 17 B, bb292fab8a18)
```

Every file beneath it is hashed and cached, and a **manifest** naming them is
stored as an object of its own — so a directory is `n + 1` ordinary objects,
moving through push, pull, and gc like any other. The manifest's hash is the
directory's identity, which makes a file edited, added, removed, or renamed
anywhere inside it show up as `modified`. Unchanged files are never re-stored,
and identical files are stored once.

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

A directory pointer adds `kind: directory` and names its manifest object; the
field is absent for files, so file pointers are unchanged.

Validation is strict — unknown fields are rejected, and `path` may not escape the
repository root. A pointer drives filesystem writes, so it is treated as
untrusted input.

### Inspection

`avc status` reports working-tree state (`ok`, `modified`, `missing`) and cache
state (`cached`, `cache-missing`) per artifact — a directory is re-scanned into
a manifest and reported the same way. `avc list` shows what a repository holds
and whether the remote can supply it, **without downloading bytes**; give it a
path to scope the listing, or a tracked directory to see the files inside it. `avc doctor` re-hashes cached
objects and fails on any drift.

Output is aligned ASCII, colored when the terminal wants it and plain when it
does not — a pipe, `NO_COLOR`, or `--color never`. Anything a script parses
should use `--porcelain` instead, which is tab-separated and stable.

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

### CI/CD

An AVC repository is an artifact registry: one Git repository can hold the
models, datasets, and archives of a dozen projects. A build that needs one of
them names the repository and the path, and gets only that:

```bash
avc fetch  --repo https://github.com/acme/artifacts models/bert -o .
avc verify --repo https://github.com/acme/artifacts models/bert -o .
avc list   --repo https://github.com/acme/artifacts models/       # just look
```

There is no bucket in those commands. AVC reads the pointers at that Git
reference — a shallow, text-only read; artifacts are gitignored — and learns the
object store from the repository's own `.avc/config.toml`. Move the bucket and
consumers do not change, because they never named it.

A job gets what it asked for and nothing around it. `models/bert` arrives as
`./bert`, not `./models/bert`: the directories above an artifact are how the
registry files it, not part of the request. And a path may reach *into* a
tracked directory — `avc fetch data/raw/2024.csv` downloads one file out of a
thousand-file dataset, because every file in a tracked directory is stored as an
object of its own.

Nothing is written but the artifacts: no clone in the workspace, no `.avc/`
directory, no cache. Objects are streamed and verified straight to where they
were asked for, `fetch` only ever issues `GetObject` so a consuming job can hold
a read-only policy, and a re-run is cheap because a file already hashing to what
its pointer claims is left alone. `avc verify` looks where `fetch` wrote and
re-checks it against a tag without contacting the store at all, which makes it a
gate.

See [CI/CD](docs/ci-cd.md) for GitHub Actions, GitLab CI, Docker, and Kubernetes
workflows, caching between jobs, and least-privilege policies.

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
3. `~/.aws/credentials` for the active profile
4. The tracked `.avc/config.toml`, which may name a `region` and a `profile` —
   names, never secrets

See [Configuration](docs/configuration.md#credentials) for the full table.

### Corporate networks and custom certificates

A proxy that inspects TLS re-signs every connection with a certificate authority
private to your organization, and a client carrying only the public roots
rejects it. Point AVC at the CA that should be trusted:

```bash
export AVC_SYSTEM_CERTS=1                          # use the machine's own trust store
export AVC_CA_BUNDLE=/etc/ssl/corporate-root.pem   # or name a PEM bundle
```

`AWS_CA_BUNDLE` and `SSL_CERT_FILE` are honored too, so a machine already set up
for the AWS CLI or `curl` needs nothing further, and the same paths can be
written into the gitignored `config.local.toml`. A rejected certificate says
which of these to reach for rather than leaving you to guess. There is no option
to disable verification. See
[TLS and corporate proxies](docs/configuration.md#tls-and-corporate-proxies).

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
