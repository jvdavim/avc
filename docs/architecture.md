# Architecture

A map of the codebase for people who want to change it. Pair this with
[Concepts](concepts.md), which covers the *what*; this page covers the *where*.

## Workspace layout

```text
avc/
├── Cargo.toml                  # workspace: members, shared deps, MSRV, license
├── Cargo.lock                  # committed — this workspace ships a binary
└── crates/
    ├── avc-core/               # library: domain types and validation
    │   └── src/
    │       ├── lib.rs          # re-exports, Error enum, unit tests
    │       ├── config.rs       # Provider, RemoteConfig, URL parsing
    │       ├── hashing.rs      # streaming SHA-256
    │       ├── object.rs       # ObjectId, cache key derivation
    │       ├── path.rs         # repository path validation and normalization
    │       ├── pointer.rs      # Pointer, ObjectMetadata, canonical YAML
    │       ├── tree.rs         # Tree, TreeEntry, directory manifests
    │       └── remote/         # object transport
    │           ├── mod.rs          # ObjectStore trait, key layout, dispatch
    │           ├── file.rs         # file:// backend
    │           ├── s3.rs           # S3 and S3-compatible REST transport
    │           ├── sigv4.rs        # AWS Signature Version 4
    │           ├── credentials.rs  # credential, region, endpoint resolution
    │           └── xml.rs          # minimal reader for S3 responses
    └── avc-cli/                # binary: the `avc` command
        ├── src/main.rs         # clap definitions and the repository workflows
        ├── src/ci.rs           # fetch and verify: the CI/CD commands
        ├── src/registry.rs     # pointers and config, from Git or from disk
        ├── src/git.rs          # shallow reads of a repository reference
        ├── src/ui.rs           # ASCII tables, color detection, message vocabulary
        └── src/progress.rs     # a bar for a terminal, periodic lines for a log
```

The split is deliberate: **`avc-core` knows nothing about the filesystem layout
of a repository or about commands.** It holds the format contract and the
transport. `avc-cli` holds the workflows. That boundary is what lets a new
adapter — or a future library API — be added without touching the format rules.

## `avc-core`

Domain logic plus transport. Everything outside `remote/` is pure: its only I/O
is reading bytes to hash them.

### `object.rs` — content addresses

`ObjectId` wraps a validated SHA-256 digest. Construction is the only way in, and
it enforces 64 hex characters, lowercasing on the way through, so an invalid
digest cannot exist as an `ObjectId` anywhere in the program.

`cache_key()` derives `objects/sha256/<first-two>/<full>`. Both the local cache
and every remote use this one function, which is what keeps the two layouts
identical by construction rather than by convention.

### `hashing.rs` — streaming digests

`hash_reader` pulls through a fixed 64 KiB stack buffer and returns
`HashResult { object, size }`. Memory use is constant regardless of file size —
the property that makes multi-gigabyte artifacts viable. Size accumulation uses
`checked_add`, so a pathological reader cannot silently wrap the counter.

`hash_file` is the thin path-taking wrapper.

### `path.rs` — the security boundary

`validate_repo_path` rejects absolute paths, `..`, `.`, backslashes, Windows
prefixes, and empty strings. This is what stops a hostile pointer file from
directing a write outside the repository — a pointer is attacker-controlled input
the moment you clone someone else's repository.

`normalize_repo_path` converts `\` to `/` and validates. `pointer_path` appends
`.avc`.

> When touching this module, add a test for the case you are changing. Path
> validation is the highest-consequence code in the workspace.

### `pointer.rs` — the format

`Pointer` and `ObjectMetadata` are `serde` structs with `#[serde(deny_unknown_fields)]`.
That attribute is the SPEC's "unknown fields are rejected by policy before format
freeze" rule, enforced by the type system.

Field declaration order *is* the serialization order, so `serialize_canonical`
produces byte-identical output for identical input. The test
`pointer_serialization_is_stable_and_round_trips` asserts the exact expected
string — if you reorder fields, that test fails, which is the intent.

`validate()` runs on both parse and serialize, so an invalid pointer can be
neither read nor written.

### `config.rs` — remote URLs

`Provider` is the closed set `File | S3 | Gcs | Azure`. `RemoteConfig::from_url`
matches on scheme and nothing else, then decomposes host and path per provider.
The `s3+https` and `s3+http` branches are the ones with real logic: host (with
port, if any) becomes `endpoint_url`, the first path segment becomes the bucket,
and the remainder becomes the prefix.

### `tree.rs` — directory manifests

A tracked directory has no storage of its own: its object is a `Tree`, a sorted,
unique list of `TreeEntry` values whose paths are relative to the directory
rather than to the repository. `Tree::new` does the sorting, so a manifest never
depends on directory-iteration order and one directory has exactly one identity.

`Tree::parse` is held to the same standard as `Pointer::parse` and for the same
reason — a manifest decides where `checkout` writes. Entry paths go through
`validate_repo_path`, unknown fields are rejected, and a manifest that arrives
unsorted or with a repeated path is refused rather than normalized: it did not
come from `Tree::new`, so its hash is not the hash of the content it claims.

### `remote/` — object transport

`ObjectStore` is the whole interface: `put`, `get`, `exists`, `list`, all keyed
by `ObjectId`. **A backend never sees a repository path**, which is what keeps
the no-path-leakage guarantee structural rather than a matter of discipline.
`remote::open` dispatches on the `Provider` already decided by `RemoteConfig`;
it never inspects a hostname.

`sigv4.rs` implements AWS Signature Version 4 directly. This is a deliberate
trade: the alternative is an SDK plus an async runtime, against a signature that
is four hashes over strings the caller already holds. Content addressing makes
the expensive part free — an upload's `x-amz-content-sha256` *is* the object's
digest, so payload bytes are never read twice. Its tests assert against
`botocore`-generated vectors, so the module is checked against an independent
implementation rather than against itself.

`s3.rs` picks addressing style from configuration: virtual-hosted for Amazon S3,
path-style whenever an endpoint is set. It sets `content-length` explicitly,
because S3 rejects the chunked encoding an HTTP client would otherwise choose,
and it configures the agent to deliver 4xx and 5xx as responses so S3's own XML
error code survives into the message the user sees.

### `lib.rs` — errors and tests

One `Error` enum via `thiserror`, covering invalid object IDs, invalid paths,
invalid remotes, unsupported pointer and manifest versions, invalid manifests,
YAML failures, I/O, missing remote objects, missing credentials, unimplemented
providers, and provider failures. `Error::is_provider_failure` is what separates exit code `3` from `1`.
Public `Result<T>` alias.

Unit tests live beside the code they cover; `lib.rs` holds those for hashing,
pointers, manifests, paths, and remote scheme parsing. `tests/object_store.rs`
asserts one contract against both backends — the S3 half runs against a real
server when `AVC_TEST_S3_ENDPOINT` is set. `crates/avc-cli/tests/directory.rs`
drives the binary itself through the directory workflow, remote round trip
included.

## `avc-cli`

Six modules: `main.rs` for the repository workflows, `registry.rs` and `git.rs`
for reading a repository however it is addressed, `ci.rs` for the commands built
for a pipeline, and `ui.rs` and `progress.rs` for presentation.

### 1. Clap definitions

`Cli`, `Command`, and per-command `Args` structs. `Paths` is a shared flattened
struct for the commands that take a required path list. `--color` is a global
argument, resolved once by `ui::init` before any command runs.

### 2. Command functions

One function per subcommand: `init`, `remote`, `add`, `commit`, `status`, `list`,
`checkout`, `push`, `pull`, `remove`, `gc`, `doctor`. Each returns
`Result<(), Failure>`, where `Failure` carries the exit code the error should
produce; `main` prints `avc: {error}` to stderr and exits with it.

`add` and `commit` both delegate to `add_one`, differing only in the
`require_pointer` flag. `pull` ends by calling `checkout_selected(paths, false)`,
which is why a `pull` will not clobber a locally modified file.

### 2a. `registry.rs` and `git.rs` — where pointers come from

`Registry` is the answer to "which repository, and how do I read it". It wraps a
`Repo` — a root directory plus its `.avc/config.toml` — and hides whether that
root is the user's worktree or a temporary checkout that `git.rs` produced from
a URL. Everything downstream sees one type, so `fetch`, `verify`, and `list` are
written once rather than twice.

`git::Checkout` does a `git init` / `fetch --depth 1` / `checkout` into a
temporary directory and deletes it on `Drop`. Artifacts are gitignored, so what
lands there is pointer files and configuration: text. That is the whole reason a
consumer can name a Git URL instead of a bucket — the two halves of a repository
travel in the same commit, and reading one is cheap.

`registry::select` resolves path selectors against a set of pointers, matching a
path exactly or as a directory prefix. It is shared with `main.rs`, so
`avc push models/bert` and `avc fetch models/bert` mean the same thing, and there
is one place where "an exact match beats a prefix" is decided.

### 2b. `ci.rs` — commands built for a pipeline

`fetch` and `verify` live apart because they invert the module's central
assumption: there may be no worktree, no cache, and no local configuration. They
take a `Registry`, resolve artifact paths against an `--output` root rather than
a repository root, and write nothing but artifacts.

`Fetcher` holds the store, the arguments, and a map of which objects have
already been written this run. `Fetcher::locate` decides where an object's bytes
come from — a path this run already wrote, a `--cache` entry that verifies, or
the remote — and `Fetcher::place` acts on that decision. Separating the two is
what lets `--dry-run` report exactly the numbers a real run produces.

`verify` shares `artifact_state` with `status`; the function takes the root as a
parameter precisely so both can use it.

### 2c. `ui.rs` — presentation

Nothing in `ui.rs` knows what an artifact is. It offers a `Table` that computes
column widths in characters, `action` for a per-artifact line with a fixed verb
column, and `paint` for style application. Text and style are kept apart until
printing, because a cell padded after being wrapped in escape codes is padded to
the wrong width.

Color is resolved once into two atomics — stdout and stderr are decided
separately, since one can be a terminal while the other is a file. Every
command's `--porcelain` path bypasses `ui.rs` entirely.

### 2d. `progress.rs` — watching a transfer

One `Progress` type with two renderers, chosen once by `progress::init` from
`--progress` and the environment: a bar redrawn on stderr for a terminal, and a
line on stdout every ten seconds for a build agent, whose log is a file in which
a carriage return is just a character. A build agent is tested for ahead of the
terminal, because some of them allocate a pseudo-terminal.

`Progress::meter` wraps a reader, so a single large object moves the bar as its
bytes stream rather than jumping when it lands — the one thing that needs to
reach inside a transfer, and the reason `ObjectStore::put` takes a `&mut dyn
Read` it consumes exactly once. `clear` takes the terminal line back before a
command prints an artifact line over it, and `Drop` does the same on the way out
of an error, so a failure is never printed across a half-drawn bar.

The counts a bar measures against come from a planning pass, which is what the
`Upload` and `Download` plans in `main.rs` and `Fetcher::plan` in `ci.rs` are
for. Each moves work that used to happen inline — asking a remote what it holds,
reading a directory's manifest — ahead of the first byte, without adding a
request or a read. Only a first `pull` of a tracked directory cannot be counted
up front, since its manifest is what names the files; there the total grows as
manifests arrive, and `percent` stays below 100 until every object is accounted
for so a growing total cannot read as finished-then-not.

### 3. Filesystem helpers

| Helper | Role |
| --- | --- |
| `find_root` | Walks up from cwd to the first `.git` |
| `load_repo` / `save_config` | Reads and writes `.avc/config.toml` |
| `pointer_files` / `collect_files` | Recursive `*.avc` scan, skipping `.git` and `target` |
| `scan_directory` / `collect_artifact_files` | Hash every regular file under a tracked directory, skipping symlinks |
| `load_tree` / `write_manifest` | Read a verified manifest out of the cache; write one into it |
| `required_objects` | Expand a pointer into the objects it needs — manifest first |
| `materialize` | Write one cached object into the worktree, honoring `--force` |
| `parse_pointer` | Reads and validates one pointer |
| `cache_path` / `remote_path` | Compose a location from `ObjectId::cache_key()` |
| `choose_remote` | Resolves `--remote`, else `default_remote` |
| `write_atomic` / `copy_atomic` | Temp file → `fsync` → rename |
| `append_ignore` / `append_ignore_path` | Idempotent `.gitignore` edits |

**Every byte-producing write goes through `write_atomic` or `copy_atomic`.** New
code that writes to the cache or worktree must too — that is the mechanism behind
the SPEC's atomicity guarantee.

## Data flow: `avc add model.bin`

```text
add(paths)
  └─ load_repo()                       find .git, read .avc/config.toml
     └─ add_one(repo, "model.bin", require_pointer=false)
        ├─ normalize_repo_path()       reject traversal        [core::path]
        ├─ is_dir()/is_file()          pick file or directory tracking
        ├─ hash_file()                 streaming SHA-256       [core::hashing]
        ├─ Pointer::new()              build + validate        [core::pointer]
        ├─ cache_path()                objects/sha256/1d/1dfc… [core::object]
        ├─ copy_atomic()               only if object absent
        ├─ append_ignore_path()        add "model.bin" to .gitignore
        └─ write_atomic()              write model.bin.avc
```

## Data flow: `avc add data/`

A directory takes the same path, with one indirection: its object is a manifest
of the files beneath it rather than bytes of its own.

```text
add(paths)
  └─ add_one(repo, "data", require_pointer=false)
     └─ track_directory()
        ├─ scan_directory()            walk, skip symlinks, hash each file
        │   └─ TreeEntry::new()        path relative to data/    [core::tree]
        ├─ store_in_cache()            one object per distinct file
        ├─ Tree::new()                 sort + validate           [core::tree]
        ├─ write_manifest()            manifest becomes an object too
        ├─ Pointer::new_directory()    kind: directory           [core::pointer]
        ├─ append_ignore_path()        add "data/" to .gitignore
        └─ write_atomic()              write data.avc
```

Because a manifest is an ordinary object, `push`, `pull`, and `gc` need no
directory-specific transport — only `required_objects` to expand one pointer
into the `1 + n` objects it references.

`avc-core` supplies validation and identity; `avc-cli` supplies placement and
durability.

## Dependencies

Kept deliberately small — an artifact tool that pulls in a large dependency tree
is a supply-chain liability for the repositories it guards.

| Crate | Used for |
| --- | --- |
| `clap` (derive) | CLI parsing |
| `serde` (derive) | Pointer and config models |
| `serde_yaml` | Pointer serialization |
| `toml` | Config serialization |
| `sha2` | SHA-256 |
| `thiserror` | Error enum |
| `url` | Remote URL parsing |

Versions are pinned in `[workspace.dependencies]` and inherited by both crates
with `dep.workspace = true`. Adding a dependency is a design decision — raise it
in an issue first. See [Contributing](contributing.md).

## Invariants to preserve

Changes that break any of these need a SPEC change first, discussed in an issue:

1. `serialize_canonical` output is byte-stable for identical input.
2. Unknown pointer fields are rejected.
3. Object keys never contain user paths.
4. All cache and worktree writes are atomic.
5. Cache reads verify size *and* digest.
6. `checkout` never overwrites differing content without `--force`.
7. No command deletes remote data.
8. Providers are chosen by scheme only.
9. Hashing memory does not scale with file size.
10. A directory's manifest is canonical — sorted, unique, and relative to the
    directory — so one directory has one identity.
11. A manifest is verified against its pointer before it is parsed, and its
    entry paths are validated before they are joined onto a worktree path.

## Known structural gaps

Honest accounting of where the current shape will need to change — several are
good first contributions, see [Roadmap](roadmap.md):

- **`main.rs` holds every repository workflow** in one file. `ci.rs` and `ui.rs`
  split off the pieces with the clearest boundaries; the remaining commands
  still want a module per group once they grow further.
- **`parse_pointer` calls `find_root()` on every invocation**, so scanning N
  pointers walks the directory tree N times. Harmless at current scale, wasteful
  at large ones.
- **Thin CLI integration tests.** `crates/avc-cli/tests/directory.rs` drives the
  binary end to end for directory artifacts and `tests/ci.rs` does the same for
  `fetch` and `verify`; the single-file repository workflows still have no
  equivalent.
- **`gc` reachability ignores Git history**, considering only worktree pointers.
- **Retries and timeouts are absent** in the S3 transport: one attempt, no deadline.
- **`--repo` shells out to `git`** and checks out a whole shallow commit to read
  pointer files. A sparse checkout, or reading blobs with `cat-file --batch`,
  would avoid writing the repository's non-pointer files to a temporary
  directory. Correct today, wasteful on a registry that also carries code.

## Where to make a change

| You want to change… | Start in |
| --- | --- |
| The pointer format | `crates/avc-core/src/pointer.rs` **and** `SPEC.md` |
| The directory manifest format | `crates/avc-core/src/tree.rs` **and** `SPEC.md` |
| Hash algorithm or chunking | `crates/avc-core/src/hashing.rs`, `object.rs` |
| Path safety rules | `crates/avc-core/src/path.rs` |
| A new remote scheme | `crates/avc-core/src/config.rs` |
| Command behavior | `crates/avc-cli/src/main.rs` |
| `fetch` or `verify` | `crates/avc-cli/src/ci.rs` **and** `docs/ci-cd.md` |
| How a repository is located or read | `crates/avc-cli/src/registry.rs`, `git.rs` |
| Path selection rules | `registry::select` — one function, every command |
| Output formatting or color | `crates/avc-cli/src/ui.rs` |
| Cache or remote layout | `ObjectId::cache_key()` — one function, both users |
