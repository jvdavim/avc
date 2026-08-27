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
    │       └── pointer.rs      # Pointer, ObjectMetadata, canonical YAML
    └── avc-cli/                # binary: the `avc` command
        └── src/main.rs         # clap definitions and all command workflows
```

The split is deliberate: **`avc-core` knows nothing about the filesystem layout
of a repository or about commands.** It holds the format contract. `avc-cli`
holds the workflows. That boundary is what will let cloud adapters and a future
library API be built without rewriting the format rules.

## `avc-core`

Pure domain logic. Its only I/O is reading bytes to hash them.

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
The `s3+https` branch is the one with real logic: host becomes `endpoint_url`,
the first path segment becomes the bucket, the remainder becomes the prefix.

### `lib.rs` — errors and tests

One `Error` enum via `thiserror`, covering invalid object IDs, invalid paths,
invalid remotes, unsupported pointer versions, YAML failures, and I/O. Public
`Result<T>` alias.

The five unit tests live at the bottom of `lib.rs` and cover streaming hash
correctness, canonical round-tripping, rejection of malformed pointers, Unicode
paths, and remote scheme parsing.

## `avc-cli`

One file, `main.rs`, in three layers.

### 1. Clap definitions

`Cli`, `Command`, and per-command `Args` structs. `Paths` is a shared flattened
struct for the commands that take a required path list.

### 2. Command functions

One function per subcommand: `init`, `remote`, `add`, `commit`, `status`, `list`,
`checkout`, `push`, `pull`, `remove`, `gc`, `doctor`. Each returns
`Result<(), String>`; `main` prints `avc: {error}` to stderr and exits `1`.

`add` and `commit` both delegate to `add_one`, differing only in the
`require_pointer` flag. `pull` ends by calling `checkout(paths, false)`, which is
why a `pull` will not clobber a locally modified file.

### 3. Filesystem helpers

| Helper | Role |
| --- | --- |
| `find_root` | Walks up from cwd to the first `.git` |
| `load_repo` / `save_config` | Reads and writes `.avc/config.toml` |
| `pointer_files` / `collect_files` | Recursive `*.avc` scan, skipping `.git` and `target` |
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
        ├─ is_file()                   reject directories
        ├─ hash_file()                 streaming SHA-256       [core::hashing]
        ├─ Pointer::new()              build + validate        [core::pointer]
        ├─ cache_path()                objects/sha256/1d/1dfc… [core::object]
        ├─ copy_atomic()               only if object absent
        ├─ append_ignore_path()        add "model.bin" to .gitignore
        └─ write_atomic()              write model.bin.avc
```

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

## Known structural gaps

Honest accounting of where the current shape will need to change — several are
good first contributions, see [Roadmap](roadmap.md):

- **No transfer abstraction.** `push`, `pull`, and `list` each check
  `matches!(remote.provider, Provider::File)` and inline the copy. A provider
  trait needs to be extracted before any cloud adapter is written.
- **`main.rs` is ~590 lines** and holds every workflow. It wants splitting into a
  module per command group once it grows further.
- **`parse_pointer` calls `find_root()` on every invocation**, so scanning N
  pointers walks the directory tree N times. Harmless at current scale, wasteful
  at large ones.
- **No integration tests.** All five tests are unit tests in `avc-core`. Nothing
  exercises the CLI end to end.
- **`gc` reachability ignores Git history**, considering only worktree pointers.
- **Exit codes 1 and 3 are not distinguished**, because no provider can fail yet.

## Where to make a change

| You want to change… | Start in |
| --- | --- |
| The pointer format | `crates/avc-core/src/pointer.rs` **and** `SPEC.md` |
| Hash algorithm or chunking | `crates/avc-core/src/hashing.rs`, `object.rs` |
| Path safety rules | `crates/avc-core/src/path.rs` |
| A new remote scheme | `crates/avc-core/src/config.rs` |
| Command behavior or output | `crates/avc-cli/src/main.rs` |
| Cache or remote layout | `ObjectId::cache_key()` — one function, both users |
