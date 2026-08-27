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
    │       └── remote/         # object transport
    │           ├── mod.rs          # ObjectStore trait, key layout, dispatch
    │           ├── file.rs         # file:// backend
    │           ├── s3.rs           # S3 and S3-compatible REST transport
    │           ├── sigv4.rs        # AWS Signature Version 4
    │           ├── credentials.rs  # credential, region, endpoint resolution
    │           └── xml.rs          # minimal reader for S3 responses
    └── avc-cli/                # binary: the `avc` command
        └── src/main.rs         # clap definitions and all command workflows
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
invalid remotes, unsupported pointer versions, YAML failures, I/O, missing
remote objects, missing credentials, unimplemented providers, and provider
failures. `Error::is_provider_failure` is what separates exit code `3` from `1`.
Public `Result<T>` alias.

Unit tests live beside the code they cover; `lib.rs` holds those for hashing,
pointers, paths, and remote scheme parsing. `tests/object_store.rs` asserts one
contract against both backends — the S3 half runs against a real server when
`AVC_TEST_S3_ENDPOINT` is set.

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

- **`main.rs` is ~700 lines** and holds every workflow. It wants splitting into a
  module per command group once it grows further.
- **`parse_pointer` calls `find_root()` on every invocation**, so scanning N
  pointers walks the directory tree N times. Harmless at current scale, wasteful
  at large ones.
- **No CLI integration tests.** `avc-core` has unit tests and an `ObjectStore`
  contract suite, but nothing drives the `avc` binary end to end.
- **`gc` reachability ignores Git history**, considering only worktree pointers.
- **Retries and timeouts are absent** in the S3 transport: one attempt, no deadline.

## Where to make a change

| You want to change… | Start in |
| --- | --- |
| The pointer format | `crates/avc-core/src/pointer.rs` **and** `SPEC.md` |
| Hash algorithm or chunking | `crates/avc-core/src/hashing.rs`, `object.rs` |
| Path safety rules | `crates/avc-core/src/path.rs` |
| A new remote scheme | `crates/avc-core/src/config.rs` |
| Command behavior or output | `crates/avc-cli/src/main.rs` |
| Cache or remote layout | `ObjectId::cache_key()` — one function, both users |
