# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While AVC is `0.x`, the on-disk formats are provisional and a minor release may
include a breaking format change. See
[format freeze](docs/roadmap.md#format-freeze).

## [Unreleased]

### Added

- **S3 compatibility layer.** `push`, `pull`, and `list` now transfer bytes to
  Amazon S3 and to any S3-compatible service — MinIO, Cloudflare R2, Ceph,
  Backblaze B2 — via `s3://`, `s3+https://`, and the new `s3+http://` scheme.
- Provider-neutral `ObjectStore` trait (`put`, `get`, `exists`, `list`) with the
  `file://` backend reimplemented against it, so both backends are held to one
  contract by a shared test suite.
- AWS Signature Version 4 request signing, implemented directly rather than via
  a cloud SDK, and verified against `botocore` reference vectors across five
  request shapes. Because object keys are content-addressed, an upload's
  `x-amz-content-sha256` is the object's own digest and payload bytes are never
  read twice.
- Credential resolution for S3 remotes: environment variables, then
  `.avc/config.local.toml`, then `~/.aws/credentials` for `$AWS_PROFILE`.
  Region and endpoint resolve on the same precedence. Temporary credentials
  (`AWS_SESSION_TOKEN`) are supported.
- `.avc/config.local.toml` is now read. It accepts per-remote `endpoint_url`,
  `region`, `access_key_id`, `secret_access_key`, `session_token`, `profile`,
  and `force_path_style`.
- Verified streaming downloads: a pulled object is hashed as it is written and
  checked against its pointer before entering the cache, so a truncated or
  tampered object is rejected rather than stored.
- `avc push` skips objects the remote already holds, making a repeated push a
  no-op instead of a re-upload.
- Exit code `3` for provider and operational failures, as `SPEC.md` reserves.
- Project documentation under `docs/`: getting started, concepts, CLI reference,
  configuration, architecture, contributing, development, and roadmap.
- Open source project files: `LICENSE` (MIT), `CONTRIBUTING.md`,
  `CODE_OF_CONDUCT.md`, `SECURITY.md`, `CHANGELOG.md`, and `.editorconfig`.
- GitHub CI workflow running fmt, clippy, tests on Linux/macOS/Windows, an MSRV
  check, and a doc build.
- Issue and pull request templates, and Dependabot configuration.

## [0.1.0]

Initial prototype release. Iteration 0 — formats are provisional.

### Added

- `avc init` — initialize AVC in a Git worktree
- `avc remote add` / `avc remote list` — configure object store remotes
- `avc add` — track a file, hash it, cache it, and write a pointer
- `avc commit` — record a new version of an already-tracked artifact
- `avc status` — report working-tree and cache state
- `avc list` — show tracked artifacts and remote availability without downloading
- `avc push` / `avc pull` — transfer objects to and from a remote
- `avc checkout` — materialize artifacts from the cache
- `avc remove` — stop tracking an artifact
- `avc gc` — delete unreferenced cache objects
- `avc doctor` — verify repository, pointer, and cache integrity
- SHA-256 content addressing, streamed in bounded memory
- Canonical, byte-stable YAML pointer serialization with strict validation
- Local content-addressed cache with atomic writes and verified reads
- Explicit remote URL schemes: `file://`, `s3://`, `s3+https://`, `gs://`, `az://`
- Automatic `.gitignore` management

### Known limitations

- Only `file://` remotes transfer bytes; cloud adapters return an explicit
  unsupported-adapter error. *(Resolved for S3 in Unreleased.)*
- `avc gc` computes reachability from worktree pointers only, so objects
  referenced solely by another branch or by history are treated as unreachable.
- `avc gc --remote` is accepted but ignored.
- `avc list` requires a `file://` remote. *(Resolved for S3 in Unreleased.)*
- Directories cannot be tracked; only regular files.
- `avc status` re-hashes every artifact on each run.
- Exit code `3` is reserved by `SPEC.md` but not yet emitted.
  *(Resolved in Unreleased.)*

[Unreleased]: https://github.com/jvdavim/avc/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jvdavim/avc/releases/tag/v0.1.0
