# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While AVC is `0.x`, the on-disk formats are provisional and a minor release may
include a breaking format change. See
[format freeze](docs/roadmap.md#format-freeze).

## [Unreleased]

### Changed

- **`avc fetch` delivers what you named, where you asked for it.** A fetched
  path now lands in `--output` under its own name instead of at the end of the
  directories the repository files it under: `avc fetch --repo <url>
  artifacts/model1 -o .` writes `./model1`, not `./artifacts/model1`. Naming no
  path still keeps every artifact's full path, since there is no selector to
  take a parent from, and omitting `--output` inside a checkout still restores
  artifacts to the paths their pointers name — `avc fetch --ref v1.0.0 --force`
  puts an old version back where it lives. `avc verify` uses the same rule, so
  it looks where `fetch` wrote. Two paths that would write different files to
  one destination are refused before anything is written. **Breaking** for a
  pipeline that depended on the old layout.

### Added

- **A path may name part of a tracked directory.** `avc fetch
  data/nested/weights.bin` downloads that one file, and `avc fetch data/nested`
  that one subdirectory, even though `data` is a single directory artifact.
  Nothing new is stored to make this work — every file inside a tracked
  directory has always been an object of its own — so this is available for
  artifacts published by earlier versions. `avc verify` and `avc list` accept
  the same paths; the commands that maintain a repository (`push`, `pull`,
  `checkout`, `commit`, `remove`) work on whole artifacts and say so rather than
  half-doing it.
- **`avc add` is faster on large artifacts.** It hashes and stores in a single
  pass over the bytes instead of reading the file once to hash it and again to
  copy it, reads in 1 MiB chunks rather than 64 KiB, and hashes the files of a
  directory in parallel across the machine's cores. No new dependency: the
  workers are scoped threads over the file list.

- **The documentation says the project is vibe coded.** README, the docs index,
  both contributing guides, and `SECURITY.md` now state plainly that nearly all
  of AVC's code, tests, and documentation were written by AI coding assistants
  under human direction and review — that none of it has been audited, and that
  documentation describing behavior the code does not have should be reported as
  the bug it is.
- **Custom certificate authorities, for networks that inspect TLS.** A proxy
  that terminates TLS re-signs it with a private CA, which the built-in Mozilla
  roots reject. `AVC_SYSTEM_CERTS=1` verifies against the machine's own trust
  store instead; `AVC_CA_BUNDLE` names a PEM bundle, as do the pre-existing
  `AWS_CA_BUNDLE` and `SSL_CERT_FILE` that a managed machine usually already
  sets; `ca_bundle` and `use_system_certs` in `.avc/config.local.toml` make
  either permanent for one repository. A bundle is read and validated when the
  command starts, so a wrong path is reported as a wrong path, and a rejected
  certificate names the setting that fixes it. There is deliberately no way to
  disable verification. See
  [TLS and corporate proxies](docs/configuration.md#tls-and-corporate-proxies).
  Reading the system trust store enables one HTTP-client feature flag, which
  adds three transitive crates and no direct dependency.
- **A remote records its region and its AWS profile.** `avc remote add
  --region <region>` pins the SigV4 signing region in the tracked
  `.avc/config.toml`, and `--profile <name>` names the section of `~/.aws/config`
  and `~/.aws/credentials` to authenticate with. Both are names rather than
  secrets, so a clone reaches the right bucket in the right region through the
  right profile with no local setup; `AWS_REGION` / `AWS_PROFILE` and
  `.avc/config.local.toml` still override them. `avc remote list` grows `REGION`
  and `PROFILE` columns when a remote configures one.
- **CI/CD commands.** `avc fetch` downloads the artifacts at a path inside a
  repository, to the paths their pointers name — no clone, no `avc init`, no
  local cache, and `s3:GetObject` as its only S3 permission. `avc verify`
  re-hashes artifacts on disk against their pointers and exits `1` on any drift,
  using nothing but the two, so a pipeline can gate on it. `fetch` also has
  `--cache` for a runner that caches a directory between jobs, `--dry-run`, and
  `--force`. See the new [CI/CD guide](docs/ci-cd.md).
- **A repository is addressed by its Git URL, not by its object store.**
  `--repo <git-url>` and `--ref <rev>` (or `$AVC_REPO` and `$AVC_REF`) make
  `fetch`, `verify`, and `list` read pointers from a shallow, text-only read of
  one revision, and read the object store out of the `.avc/config.toml` that
  came with them. A consumer never names a bucket, a prefix, or an endpoint, so
  moving storage does not break them; `--remote-url` overrides it for a single
  run. Reading from a Git URL requires the `git` command.
- **Any revision of a registry can be named.** `--ref` takes anything that
  names one commit: a branch, a tag, `HEAD` for the default branch, a commit id
  whole *or abbreviated*, or a fully qualified `refs/heads/…` or `refs/tags/…`
  name for a repository where a branch and a tag collide. An abbreviated id is
  resolved by fetching every branch and tag without their file contents, since a
  prefix is not a name a server can look up — whole ids and tags stay a single
  shallow fetch, and are what a pipeline should use. The commit a revision
  resolved to is printed in the heading.
- **`--ref` works inside a checkout**, with no `--repo`, reading the pointers
  Git holds at that commit rather than the ones on disk — so `avc list --ref
  v1.0.0` shows what a release shipped, `avc verify --ref v1.0.0` asks whether
  the working tree still matches it, and `avc fetch --ref v1.0.0 --force`
  restores that version in place. Artifacts belong to the worktree, not to the
  temporary checkout the pointers were read out of. Omitting `--ref` is
  deliberately *not* the same as `--ref HEAD`: with no revision a local
  repository is read off the working tree, so an uncommitted pointer still
  counts, as it does everywhere else.
- **Path selection inside a repository**, shared by `commit`, `push`, `pull`,
  `checkout`, `remove`, `fetch`, `verify`, and `list`. A path matches an
  artifact exactly or acts as a directory prefix naming everything beneath it,
  so `avc fetch models/bert` takes one project out of a registry holding a
  hundred. An exact match beats a prefix, a trailing `/` is optional, and a
  trailing `.avc` is stripped so a pointer path from `git diff --name-only`
  names its artifact unchanged. A path matching nothing is an error rather than
  an empty selection — previously only `push`, `pull`, and `checkout` accepted
  paths, and only exact ones.
- **`avc list` takes a path.** With a prefix it lists the artifacts beneath it;
  with a tracked directory named exactly it lists the files stored inside that
  directory rather than the single row they collapse to. With `--repo` it
  browses a repository without cloning it.
- `--porcelain` on `status`, `list`, `fetch`, and `verify`: tab-separated
  records with no header, summary, or color. This is now the stable interface
  for scripts; the human-facing output is explicitly not.
- Terminal-aware color, honoring `--color <auto|always|never>`, `AVC_COLOR`,
  `NO_COLOR`, `CLICOLOR_FORCE`, and `TERM=dumb`. Color is decoration only:
  every line reads identically without it.
- **Transfer progress** on `push`, `pull`, and `fetch`, in the form the run
  calls for. At a terminal it is a bar on stderr — percentage, bytes, the file
  moving, rate, and an estimate — redrawn in place and erased when the command
  finishes, so it never lands in a redirect and never scrolls the artifact lines
  away. In a CI pipeline it is an ordinary stdout line every ten seconds
  instead, since a log is a file read after the fact and a redrawn line is
  thousands of unreadable fragments in it. A pipeline is recognized by `CI`,
  `CONTINUOUS_INTEGRATION`, `GITHUB_ACTIONS`, `GITLAB_CI`, `JENKINS_URL`,
  `TEAMCITY_VERSION`, or `TF_BUILD`, ahead of the terminal test because some
  runners allocate a pseudo-terminal; a pipe, a redirect, and `TERM=dumb` are
  treated the same way. `--progress <auto|always|never>` and `AVC_PROGRESS`
  override it, `--porcelain` suppresses it, and the summary each command prints
  still says everything progress said. `$COLUMNS` sets the width, or 80.
- `AVC_REPO`, `AVC_REF`, and `AVC_CACHE_DIR` environment variables, so a
  pipeline can configure the CI/CD commands once rather than on every command
  line. A `--repo` URL containing `user:password@` is redacted everywhere AVC
  prints it, including inside Git's own error messages.
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
