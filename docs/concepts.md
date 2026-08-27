# Concepts

AVC has four moving parts: **pointers**, **objects**, the **cache**, and
**remotes**. Everything the CLI does is a combination of those four.

## The core idea

Git is excellent at versioning text and terrible at versioning gigabytes. AVC
splits the problem:

| Concern | Owned by |
| --- | --- |
| *Which* version of an artifact belongs to this commit | Git, via the pointer file |
| *Where* the bytes live and how they are verified | AVC, via content addressing |

Git never sees artifact bytes. AVC never invents a history model. The pointer is
the seam between them.

## Pointers

A pointer is a small UTF-8 YAML file that names one artifact by content. It sits
next to the artifact with `.avc` appended: `model.bin` → `model.bin.avc`.

```yaml
version: 1
path: model.bin
object:
  algorithm: sha256
  hash: 1dfc4d103921b3462e1c482b3019f6e1838ec62eb9dbd67ffe4602325dd82fe2
  size: 17
  media_type: application/octet-stream
```

`media_type` is optional metadata. `avc add` does not infer it, so pointers it
writes carry `media_type: null`; the field is emitted either way to keep field
order fixed.

Field order is fixed, line endings are LF, and no timestamps appear anywhere.
That makes serialization canonical: the same artifact always produces a
byte-identical pointer, so Git diffs stay meaningful and merges stay tractable.

Validation is strict, and deliberately so — a pointer is a security boundary
because it drives filesystem writes:

- `version` must equal `1`.
- `path` must be repository-relative: no leading `/`, no `..`, no `.`, no
  backslashes, no Windows drive prefixes.
- `algorithm` must equal `sha256`.
- `hash` must be exactly 64 hexadecimal characters.
- `size` must be present and fit in a `u64`.
- **Unknown fields are rejected.** Before the format is frozen, an unrecognized
  key means the pointer came from a version AVC does not understand, and
  guessing would be worse than failing.

Non-ASCII paths are fully supported: `données/模型.bin` produces
`données/模型.bin.avc`.

## Objects

An object is the immutable content of one artifact version, identified by the
SHA-256 of its exact bytes. Its logical key is:

```text
objects/sha256/<first-two-hash-characters>/<full-hash>
```

Three properties follow from this:

- **Deduplication.** Identical bytes have one key. The same 4 GB checkpoint
  referenced from ten paths or ten branches is stored once.
- **Immutability.** A valid object at a key is never rewritten. Content
  addressing means "different bytes" always means "different key."
- **No path leakage.** The key contains no user path. A shared bucket learns
  hashes and sizes, never your repository's directory structure.

The two-character shard prefix keeps directory fan-out manageable on filesystems
and object stores that degrade with very wide directories.

Hashing streams through a 64 KiB buffer, so hashing a 100 GB file uses the same
memory as hashing a 100 byte one.

## The cache

`.avc/cache/` is a local content-addressed store, laid out by object key:

```text
.avc/
  config.toml            # tracked: provider, bucket, prefix, endpoint, remotes
  config.local.toml      # ignored: local credential overrides
  cache/
    objects/sha256/1d/1dfc4d10…
  state/
```

The cache is the working set. `add` writes into it, `checkout` materializes from
it, `push` uploads from it, `pull` downloads into it. `.avc/cache/` and
`.avc/config.local.toml` are added to `.gitignore` by `avc init`.

Every write is atomic: bytes go to a temporary sibling file, are `fsync`ed, then
renamed into place. An interrupted `add` or `pull` leaves a stray temp file, never
a half-written object that would later be trusted as valid.

Every read verifies both size and hash. A silently corrupted cache entry is
detected rather than served.

## Remotes

A remote is an object store that mirrors the cache layout. Providers are chosen
by **URL scheme only** — never inferred from a hostname, because guessing a
provider from a domain is how credentials end up sent to the wrong endpoint.

| Scheme | Provider | Status in `0.1.0` |
| --- | --- | --- |
| `file://` | Local directory | **Implemented** |
| `s3://` | Amazon S3 | Parses and stores; transfers unimplemented |
| `s3+https://` | S3-compatible (MinIO, R2, Ceph) | Parses and stores; transfers unimplemented |
| `gs://` | Google Cloud Storage | Parses and stores; transfers unimplemented |
| `az://` | Azure Blob Storage | Parses and stores; transfers unimplemented |

Any other scheme, including a bare `https://`, is rejected.

See [Configuration](configuration.md) for how each URL decomposes into bucket,
prefix, and endpoint.

## Artifact states

`avc status` reports two independent axes per artifact.

**Working tree**, from re-hashing the file on disk:

| State | Meaning |
| --- | --- |
| `ok` | The file exists and its hash matches the pointer |
| `modified` | The file exists but its content differs from the pointer |
| `missing` | No file at that path |

**Cache**:

| State | Meaning |
| --- | --- |
| `cached` | The referenced object is in `.avc/cache` |
| `cache-missing` | It is not; `avc pull` is needed before checkout |

`missing` + `cached` is the normal state right after a fresh clone plus pull of
the cache. `ok` + `cache-missing` happens when you produced the file locally
after a `gc`.

## Lifecycle

```text
   working file
        │  avc add / avc commit
        ▼
   hash (sha256, streamed)
        │
        ├──► .avc/cache/objects/…      (bytes, content-addressed)
        └──► model.bin.avc             (pointer, committed to Git)
                 │
                 │  git push / git pull
                 ▼
            other clone
                 │  avc pull
                 ▼
        remote ──► cache ──► working file
                          avc checkout
```

`pull` is `download-to-cache` followed by `checkout`. `checkout` never touches
the network; it only materializes from cache.

## Safety model

Four rules the implementation upholds:

1. **Dirty files are never clobbered.** `checkout` re-hashes an existing target
   and refuses to overwrite content that differs from the pointer, unless
   `--force` is given.
2. **Writes are atomic.** Temp file, `fsync`, rename.
3. **Reads are verified.** Size and SHA-256 are both checked.
4. **No remote deletion.** No command in `0.1.0` deletes remote data. `gc`
   affects the local cache only.

## Relationship to Git LFS

| | Git LFS | AVC |
| --- | --- | --- |
| Pointer location | Replaces the file in Git's index | Sibling `.avc` file; artifact is gitignored |
| Materialization | Automatic via smudge/clean filters | Explicit `avc pull` / `avc checkout` |
| Server requirement | LFS-aware Git server or batch API | Any object store; no Git server support needed |
| Setup per clone | `git lfs install` | None beyond the binary |
| Partial fetch | `git lfs pull --include` | `avc pull <path>` |

The trade-off is deliberate: AVC gives up transparent checkout to avoid needing
anything special from the Git server or from every contributor's Git install.

## Next

- [CLI Reference](cli.md) — commands, flags, exit codes
- [Architecture](architecture.md) — how the crates implement all of this
- [`SPEC.md`](../SPEC.md) — the normative contract
