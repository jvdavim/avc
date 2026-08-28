# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While AVC is `0.x`, the on-disk formats are provisional and a minor release may
include a breaking format change. See
[format freeze](docs/roadmap.md#format-freeze).

## [Unreleased]

### Added

- **CI/CD commands.** `avc fetch` downloads artifacts straight from a remote to
  the paths their pointers name — no Git repository, no `avc init`, no local
  cache, and `s3:GetObject` as its only S3 permission. `avc verify` re-hashes
  artifacts on disk against their pointers and exits `1` on any drift, using
  nothing but the two, so a pipeline can gate on it. Both select artifacts from
  arguments, a directory scan, or stdin, take their remote from `--remote-url`
  or `$AVC_REMOTE_URL`, and offer `--porcelain`. `fetch` also has `--cache` for
  a runner that caches a directory between jobs, `--dry-run`, and `--force`.
  See the new [CI/CD guide](docs/ci-cd.md).
- `--porcelain` on `status`, `list`, `fetch`, and `verify`: tab-separated
  records with no header, summary, or color. This is now the stable interface
  for scripts; the human-facing output is explicitly not.
- Terminal-aware color, honoring `--color <auto|always|never>`, `AVC_COLOR`,
  `NO_COLOR`, `CLICOLOR_FORCE`, and `TERM=dumb`. Color is decoration only:
  every line reads identically without it.
- `AVC_REMOTE_URL` and `AVC_CACHE_DIR` environment variables, so a pipeline can
  configure `avc fetch` once rather than on every command line.
- **Directory artifacts.** `avc add data/` tracks a whole directory as one
  artifact with one pointer, the way `dvc add data/` does. Every file beneath it
  is hashed and cached, and a manifest naming them is stored as an object of its
  own, so the directory's identity is that manifest's digest. `status`,
  `commit`, `push`, `pull`, `checkout`, `list`, `gc`, and `doctor` all
  understand directories; `push` and `pull` order the manifest so a remote never
  holds one that names bytes it lacks. Files are deduplicated across the whole
  repository, so re-versioning a thousand-file directory after one edit stores
  one new file object and one new manifest.
- Optional `kind` field in the pointer format, `file` or `directory`. It is
  omitted for files, so existing file pointers keep byte-identical output and
  parse unchanged.
- Directory manifest format (`version: 1`, sorted unique entries relative to the
  tracked directory) — see [`SPEC.md`](SPEC.md#directory-format).
- A trailing slash is accepted wherever a path is: `avc add data/` and
  `avc add data` name the same artifact.
- CLI integration tests (`crates/avc-cli/tests/directory.rs`) driving the binary
  end to end, including a push/clone/pull round trip through a `file://` remote.
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

### Changed

- **Command output is reformatted.** Aligned ASCII tables for `status`, `list`,
  `verify`, and `remote list`; a fixed verb column for the per-artifact lines of
  `add`, `push`, `pull`, `checkout`, `fetch`, `gc`, and `remove`; a summary line
  counting what happened; human-readable sizes and twelve-character digests.
  `status` and `list` gained a `SIZE` column, `status` reports a per-state
  count, and `push`/`pull` name the remote they are talking to. Scripts reading
  the old tab-separated `status` and `list` output should add `--porcelain`,
  which prints it unchanged apart from `list` no longer emitting a header.
- Artifacts are processed in sorted path order everywhere, so repeated runs and
  runs on different machines produce identical output.
- `avc status` collects unparseable pointers and reports them after the table
  instead of interleaving them with valid rows.
- `avc doctor` reports how many cache objects it re-hashed, and `avc gc` reports
  how many bytes it reclaimed.
- `avc gc` now fails instead of collecting when it cannot read a directory's
  manifest, because reachability would otherwise be a guess and the objects it
  deleted might still be needed. It also no longer silently skips pointers it
  cannot parse.
- `avc checkout` now reports a named path with no pointer as an error rather
  than ignoring it, matching `push` and `pull`.

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
- Directories cannot be tracked; only regular files. *(Resolved in Unreleased.)*
- `avc status` re-hashes every artifact on each run.
- Exit code `3` is reserved by `SPEC.md` but not yet emitted.
  *(Resolved in Unreleased.)*

[Unreleased]: https://github.com/jvdavim/avc/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jvdavim/avc/releases/tag/v0.1.0
