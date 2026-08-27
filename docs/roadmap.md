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
- Remote artifact listing without downloading bytes
- Automatic `.gitignore` management

### Not implemented

- Any cloud transport — S3, GCS, and Azure URLs configure but do not transfer
- Directory artifacts
- Git revision selection (`--rev`, checkout of an artifact as of an old commit)
- Concurrent or resumable transfers
- Credential resolution
- Machine-readable output
- Remote garbage collection

## Near term

### 1. Provider transfer trait

**The blocking item for everything cloud.** Today `push`, `pull`, and `list` each
test `Provider::File` inline and hard-code a filesystem copy. Extract a
provider-neutral trait — `put`, `get`, `exists`, `list` over object keys — and
reimplement the `file://` backend against it.

This must land before any adapter, or the adapters will each grow their own
divergent notion of what a transfer is.

### 2. S3 adapter

First real adapter, behind the trait above. Covers `s3://` and `s3+https://`,
which brings MinIO, Cloudflare R2, Ceph, and Backblaze B2 along with it.

Requires deciding credential precedence — see [Configuration](configuration.md).
The intent is provider-standard chains first, `.avc/config.local.toml` second, so
AVC does not become another place secrets leak from.

### 3. GCS and Azure adapters

Same trait, same shape as S3.

### 4. Exit code fidelity

`SPEC.md` reserves `3` for provider and operational failures, but the CLI
currently returns `1` for everything. Once adapters exist there is a real
distinction to draw, and scripts can rely on it.

## Medium term

### Integration test suite

Currently all five tests are `avc-core` unit tests; nothing exercises the CLI end
to end. Wanted: a harness that builds a temporary Git worktree, runs real
commands, and asserts on filesystem state and exit codes.

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

### Directory artifacts

`SPEC.md` currently rejects directories outright. Supporting them means deciding
whether a directory is a tree object or a manifest of file objects, and how
partial materialization works. Needs a design discussion before code.

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
3. Directory-artifact design is settled — even if the decision is "never" —
   because it may need a pointer field.

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
