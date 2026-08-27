# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While AVC is `0.x`, the on-disk formats are provisional and a minor release may
include a breaking format change. See
[format freeze](docs/roadmap.md#format-freeze).

## [Unreleased]

### Added

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
  unsupported-adapter error.
- `avc gc` computes reachability from worktree pointers only, so objects
  referenced solely by another branch or by history are treated as unreachable.
- `avc gc --remote` is accepted but ignored.
- `avc list` requires a `file://` remote.
- Directories cannot be tracked; only regular files.
- `avc status` re-hashes every artifact on each run.
- Exit code `3` is reserved by `SPEC.md` but not yet emitted.

[Unreleased]: https://github.com/jvdavim/avc/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jvdavim/avc/releases/tag/v0.1.0
