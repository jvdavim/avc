# Roadmap

What `0.1.0` does, what it does not, and what comes next. This page replaces the
`PLAN.md` referenced by earlier drafts.

Nothing here is a dated commitment. It is a statement of intent and an invitation
— items marked **good first issue** are scoped for a first contribution.

## Status: Iteration 0

`0.1.0` is a working local MVP. On-disk formats are provisional and remain so
until clone, branch, merge, push, pull, and recovery workflows have run against a
real repository. Until that happens, a breaking format change is possible in any
release.

### Implemented

- File-only artifact tracking
- SHA-256 content addressing, streamed in bounded memory
- Sibling pointer files (`model.bin` → `model.bin.avc`)
- Canonical, byte-stable pointer serialization
- Strict pointer validation with unknown-field rejection
- Local cache with atomic writes and verified reads
- Git worktree discovery
- `status` and `doctor` integrity checks
- Safe `checkout` with dirty-file protection
- Local garbage collection
- Offline `file://` remotes for `push` and `pull`
- Provider-neutral `ObjectStore` trait, with `file://` reimplemented against it
- S3 transport for `s3://`, `s3+https://`, and `s3+http://`, covering Amazon S3,
  MinIO, Cloudflare R2, Ceph, and Backblaze B2
- SigV4 request signing, verified against `botocore` reference vectors
- Credential resolution: environment, `.avc/config.local.toml`, `~/.aws/credentials`
- Streaming uploads and verified streaming downloads in bounded memory
- Exit code `3` for provider and operational failures
- Remote artifact listing without downloading bytes
- Automatic `.gitignore` management
- Directory artifacts — `avc add data/` tracks a whole tree as one artifact

### Not implemented

- GCS and Azure transport — `gs://` and `az://` URLs configure but do not transfer
- IAM instance roles, ECS task roles, SSO, and `assume-role`
- Git revision selection (`--rev`, checkout of an artifact as of an old commit)
- Concurrent or resumable transfers (no multipart upload; a push restarts on failure)
- Machine-readable output
- Remote garbage collection

## Near term

### 1. GCS and Azure adapters

Same `ObjectStore` trait as S3, same shape. The trait and the credential
plumbing are in place, so these are now additive rather than architectural.

### 2. Multipart upload

A push of a 40 GB artifact is a single `PUT`. A dropped connection restarts it,
and some S3-compatible servers cap a single-part upload well below that. Add
multipart with a part size that adapts to object size.

### 3. Retries and timeouts

The S3 transport currently makes one attempt with no timeout. A transient 500 or
a stalled socket should be retried with backoff, and a hung connection should
fail rather than hang forever. **Good first issue.**

### 4. Extended credential chain

IAM instance roles, ECS task roles, and SSO. Each is an HTTP call to a metadata
endpoint that yields temporary credentials; the `x-amz-security-token` path they
need is already implemented and tested.

## Medium term

### Integration test suite

`avc-core` carries unit tests plus a shared `ObjectStore` contract suite run
against both backends — the S3 half against a real server when
`AVC_TEST_S3_ENDPOINT` is set — and `avc-cli` drives the binary through the
directory workflow and a `file://` round trip. Not yet covered: the rest of the
commands end to end, S3 transport in CI, and exit codes for provider failures.
Wanted: a harness that builds a temporary Git worktree, runs real commands, and
asserts on filesystem state and exit codes.

High value, no architectural knowledge required. **Good first issue.**

### `status` performance

`status` re-hashes every artifact on every invocation. On a repository with
hundreds of gigabytes tracked, that is minutes of I/O. Add an mtime-and-size fast
path in `.avc/state/` — the directory already exists for exactly this — with a
`--rehash` escape hatch for when you do not trust it.

### Machine-readable output

`--format json` for `status`, `list`, and `remote list`, so CI can consume AVC
without parsing tab-separated text. Would let the human-facing output evolve
freely.

**Good first issue** for `remote list`.

### Partial directory materialization

A directory is checked out whole. Fetching or materializing a subset — `avc pull
data/train` — needs a way to name a path inside a manifest, and a definition of
what `status` should report for a directory only partly on disk.

Related: `checkout` never deletes, so a file the manifest does not name is left
in place and keeps the directory reading as `modified`. Removing extras needs an
explicit, guarded flag rather than silent deletion.

### Git revision selection

Check out the artifact as of an arbitrary commit — `avc checkout --rev HEAD~5`.
Requires reading pointer files out of a Git revision rather than the worktree.

### Smarter `gc`

Today reachability comes from worktree pointers only, so objects referenced by
another branch are deleted. Compute reachability across Git refs instead, with
flags to bound how much history is considered.

This is the sharpest edge in `0.1.0` and worth fixing early.

## Longer term

- **Concurrent transfers**, with progress reporting. Uploading fifty artifacts
  serially wastes most of the available bandwidth.
- **Resumable transfers** — multipart upload and ranged download, so a dropped
  connection does not restart a 40 GB push.
- **Remote `gc`**, gated behind explicit confirmation. `SPEC.md` currently
  forbids remote deletion entirely; lifting that needs care.
- **Shallow and partial fetch** — pull only artifacts matching a path pattern.
- **A stable `avc-core` library API**, so other tools can read and write pointers
  without shelling out.
- **Shell completions** for bash, zsh, and fish. `clap` generates them; the work
  is wiring up a build step and packaging. **Good first issue.**
- **Prebuilt binaries and crates.io publication**, so installing does not require
  a Rust toolchain.

## Format freeze

The pointer format leaves "provisional" and becomes stable when:

1. Clone, branch, merge, push, pull, and recovery have run against a real
   multi-contributor repository.
2. At least one cloud adapter is in production use.
3. Directory-artifact design has settled in practice. The `kind` field and the
   manifest format landed in `0.1.0`; freezing them means committing to
   manifest-of-file-objects over a nested tree object.

At that point `version: 1` freezes and any change requires `version: 2` plus a
migration path.

## Out of scope

Deliberately not goals, so nobody builds them by surprise:

- **Replacing Git.** AVC has no history model. Versioning comes from Git.
- **Being a Git LFS client.** No LFS protocol support.
- **General file synchronization.** AVC serves Git repositories, not arbitrary
  directory trees.
- **Encryption at rest.** Use the object store's encryption. AVC will not
  implement its own crypto.

## Influencing this list

Priorities follow contributor interest. If something here matters to you, say so
in an issue — or open one for a use case that is missing.
See [Contributing](contributing.md).
