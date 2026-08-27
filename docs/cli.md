# CLI Reference

Everything `avc` can do in `0.1.0`. Behavior described here reflects the current
implementation, including its gaps — where a flag is accepted but not yet
honored, this page says so.

```text
avc init
avc remote add <name> <provider-url>
avc remote list
avc add <path> [<path>...]
avc list [--remote <name>]
avc status
avc commit <path> [<path>...] [--force]
avc push [<path>...] [--remote <name>]
avc pull [<path>...] [--remote <name>]
avc checkout [<path>...] [--force]
avc remove <path> [<path>...]
avc gc [--remote <name>] [--dry-run]
avc doctor
```

## Global behavior

**Repository discovery.** Every command except `--help` and `--version` walks up
from the current directory until it finds a `.git` entry. That directory is the
repository root, and all artifact paths are relative to it. If no `.git` is
found, the command fails with `not inside a Git worktree`.

**Initialization check.** Every command except `init` requires
`.avc/config.toml` to exist, otherwise: `AVC is not initialized; run 'avc init'`.

**Pointer discovery.** Commands that operate on "all artifacts" find them by
recursively scanning the worktree for files ending in `.avc`, skipping the `.git`
and `target` directories. Pointers are found on disk, not read out of Git's
index, so a pointer you have not committed still counts.

**Path validation.** Every path is normalized and validated: it must be
repository-relative, with no `..`, no `.`, no absolute prefix, and no backslash.

---

## `avc init`

Initialize AVC in the current Git worktree.

```bash
avc init
```

Creates `.avc/cache/`, `.avc/state/`, and `.avc/config.toml` (if absent), then
appends `.avc/cache/` and `.avc/config.local.toml` to `.gitignore` if not
already listed. Safe to run twice; it will not overwrite an existing config.

Fails if the current directory is not inside a Git worktree.

---

## `avc remote add`

Register a remote by URL.

```bash
avc remote add <name> <provider-url>
```

```bash
avc remote add origin file:///tmp/avc-remote
avc remote add origin s3://my-bucket/artifacts
avc remote add minio s3+https://storage.example.com/my-bucket/artifacts
avc remote add local s3+http://localhost:9000/my-bucket/artifacts
```

The URL is decomposed into provider, bucket/container, prefix, and optional
endpoint, then written to `.avc/config.toml`. Adding a name that already exists
**replaces** it. The first remote added becomes `default_remote`.

Only `file://`, `s3://`, `s3+https://`, `s3+http://`, `gs://`, and `az://` are
accepted. A bare `https://` URL is rejected — see
[Configuration](configuration.md).

## `avc remote list`

```bash
avc remote list
```

```text
* origin File /tmp/avc-remote
  backup S3 my-bucket
```

`*` marks the default remote.

---

## `avc add`

Track one or more files or directories.

```bash
avc add <path> [<path>...]
```

For each path: hash it, copy the bytes into the cache if that object is not
already present, append the path to `.gitignore`, and write the pointer file.

```bash
avc add model.bin data/train.parquet
```

```text
tracked model.bin (sha256:1dfc4d…)
```

Fails if the path is neither a regular file nor a directory.

Re-running `add` on a changed file updates the pointer to the new content and
adds a new cache object. The previous object remains until `gc`.

### Directories

A directory is one artifact with one pointer, the way `dvc add` treats one:

```bash
avc add data/
```

```text
tracked data/ (3 file(s), 17 B, sha256:bb292f…)
```

Every regular file beneath the directory is hashed and cached, and a **manifest**
naming them is stored as an object of its own. `data/` gets a single pointer at
`data.avc`, and `data/` is added to `.gitignore`. A trailing slash is optional
everywhere — `avc add data/` and `avc add data` name the same artifact.

The directory's identity is its manifest's hash, so a file edited, added,
removed, or renamed anywhere beneath it makes the whole artifact `modified`.
Re-run `avc commit data` to record the new contents.

Files are deduplicated across the whole repository: identical files inside a
directory, or shared with a separately tracked artifact, are stored once. See
[Concepts](concepts.md#directories) for the manifest format.

Two directories are refused rather than tracked misleadingly:

- an empty directory — `directory contains no files to track: <path>`
- one containing a `.avc` file, which pointer discovery would read as a pointer
  and the manifest would record as content

Symlinks beneath the directory are skipped, not followed.

## `avc commit`

Record a new version of an **already-tracked** artifact.

```bash
avc commit <path> [<path>...] [--force]
```

Identical to `add` — directories included — except it first requires that a
pointer already exists for the path, failing with `no pointer exists for <path>`
if not.

`--force` skips that requirement, making `commit --force` behave like `add`.

> Use `add` to start tracking, `commit` to update. The distinction guards against
> typos silently creating a new tracked artifact when you meant to update one.

---

## `avc status`

Report working-tree and cache state for every tracked artifact.

```bash
avc status
```

```text
ok      cached          model.bin
modified        cached          data/train.parquet
missing cache-missing   weights/final.safetensors
```

Tab-separated columns: working-tree state, cache state, path.

| Working tree | Meaning |
| --- | --- |
| `ok` | File exists; hash and size match the pointer |
| `modified` | File exists; content differs from the pointer |
| `missing` | No file at that path |

| Cache | Meaning |
| --- | --- |
| `cached` | Every object the artifact needs is present in `.avc/cache` |
| `cache-missing` | At least one is not; `pull` before `checkout` |

Directories are shown with a trailing slash and follow the same states: a
directory is re-scanned and re-hashed into a manifest, so `modified` covers a
file edited, added, removed, or renamed anywhere beneath it, and `missing` means
the directory itself is gone. It is `cached` only when its manifest *and* every
file the manifest names are cached, because anything less cannot be checked out.

Unparseable pointers are reported as `invalid <path>: <error>` and skipped rather
than aborting the run.

Prints `no AVC pointers found` when nothing is tracked.

**Cost note:** `status` re-hashes every existing artifact. On a repository with
hundreds of gigabytes tracked, this is I/O-bound and slow. There is no
mtime/size fast path yet — see [Roadmap](roadmap.md).

---

## `avc list`

Show tracked artifacts and their availability on a remote, **without downloading
bytes**.

```bash
avc list [--remote <name>]
```

```text
PATH    SIZE    OBJECT  REMOTE
model.bin       17      sha256:1dfc4d…       available
data/train.parquet      4194304 sha256:9a3b1c…       missing
```

Uses the default remote when `--remote` is omitted.

Availability is resolved with a single prefixed listing of the remote, not one
request per artifact, so a repository with a thousand pointers costs one round
trip. Objects in the bucket that AVC did not write are ignored.

For a directory, `SIZE` is the total bytes of the files it contains, not the size
of its manifest, and `REMOTE` reads `available` only when the manifest *and*
every file it names are on the remote — a half-uploaded directory cannot be
restored. Reading those numbers needs the manifest, so `list` fetches it from
the remote when it is not cached. A manifest is metadata of a few bytes per
file; artifact bytes are still never downloaded. When the manifest is on neither
side, the file list is unknowable and the row reads `-` and `missing`.

> **Limitation:** `gs://` and `az://` remotes fail with
> `provider adapter not implemented`.

---

## `avc push`

Upload cached objects to a remote.

```bash
avc push [<path>...] [--remote <name>]
```

With no paths, pushes every tracked artifact; otherwise only the paths given
(matched against the `path` field inside each pointer, so pass repository-relative
paths exactly as they appear in `avc status`).

Fails with `cache object missing for <path>` if the object is not in the local
cache — run `add`/`commit` or `pull` first. Naming a path that has no pointer is
an error rather than a silent no-op.

Objects already present on the remote are skipped — reported as
`up to date <path>` — because content-addressed objects are immutable and
re-uploading identical bytes is pure cost. Uploads stream in bounded memory.

A directory uploads as its files followed by its manifest — `pushed data/ (4
object(s))` — in that order, so a manifest on the remote never names bytes that
have not arrived yet. Duplicate files are uploaded once.

> **Limitation:** no multipart upload. A very large artifact is a single `PUT`,
> and a dropped connection restarts it. `gs://` and `az://` fail with
> `provider adapter not implemented`.
>
> The skip check is a `HEAD` request. S3 answers `HEAD` on a missing object with
> `403` rather than `404` when the credentials lack `s3:ListBucket`, so a
> write-only credential makes `push` fail rather than upload. Grant
> `s3:ListBucket` alongside `s3:PutObject`.

## `avc pull`

Download objects from a remote into the cache, then materialize them.

```bash
avc pull [<path>...] [--remote <name>]
```

`pull` is `download` followed by an implicit `checkout` **without** `--force`, so
it will refuse to overwrite a locally modified file. Fetch the bytes and resolve
the conflict deliberately with `avc checkout --force` if that is what you want.

Fails with `remote object not found: <hash>` if the remote lacks the object.
Objects already in the cache are not re-downloaded.

A directory downloads its manifest first — until that has arrived and been
verified, the rest of its objects are unknown — then every file the manifest
names, then materializes the tree.

Each download is hashed as it is written and checked against its pointer's size
and digest before it becomes visible in the cache. An object that does not match
is rejected with `remote object for <path> does not match its pointer`, and no
partial file is left behind.

> **Limitation:** no ranged or resumable download. `gs://` and `az://` fail with
> `provider adapter not implemented`.

---

## `avc checkout`

Materialize artifacts from the local cache into the working tree. Never touches
the network.

```bash
avc checkout [<path>...] [--force]
```

With no paths, checks out everything tracked.

If the target file exists, it is re-hashed. When the content differs from the
pointer, the command refuses:

```text
avc: refusing to replace modified file model.bin; use --force
```

`--force` skips that check and overwrites unconditionally. **This discards
uncommitted changes to the artifact.**

Fails with `cache object missing for <path>` when the object is not cached.

For a directory, the check is applied per file — `refusing to replace modified
file data/a.bin` — and checkout stops there rather than overwriting the rest.
Files present in the directory that the manifest does not name are **left
alone**: `checkout` never deletes. A directory that has been restored on top of
unrelated leftovers therefore still reads as `modified`; remove the extras
yourself.

> Naming a path that has no pointer is an error rather than a silent no-op, and
> a trailing slash is accepted: `avc checkout data/` and `avc checkout data`
> both select `data.avc`.

---

## `avc remove`

Stop tracking an artifact.

```bash
avc remove <path> [<path>...]
```

```text
untracked model.bin; artifact retained
```

Deletes the pointer file only. It does **not** delete the working file, the cache
object, or the `.gitignore` entry. Commit the pointer deletion with Git, and
remove the `.gitignore` line by hand if you want Git to track the file directly
again. Reclaim the cache object with `gc`.

---

## `avc gc`

Delete cache objects that no pointer references.

```bash
avc gc [--remote <name>] [--dry-run]
```

```bash
avc gc --dry-run     # would remove /path/.avc/cache/objects/sha256/9a/9a3b1c…
avc gc               # removed 9a3b1c…
```

Reachability is computed from pointer files **in the current working tree only**,
and spans manifests: a directory keeps its manifest object and every file object
that manifest names.

If a directory's manifest is not in the cache, its file list is unknowable, so
`gc` stops rather than guessing:

```text
avc: cache object missing for data; run `avc pull data`; refusing to delete
objects that may still be referenced
```

> **This is the sharpest edge in `0.1.0`.** Objects referenced only by another
> branch, a stash, or an older commit are treated as unreachable and deleted. If
> those objects have been pushed, `avc pull` restores them; if they have not,
> they are gone. Run `avc push` before `gc`, and prefer `--dry-run` first.

> `--remote` is accepted for forward compatibility but currently **ignored**.
> `gc` never contacts or modifies a remote.

---

## `avc doctor`

Verify repository integrity.

```bash
avc doctor
```

```text
doctor: repository, pointers, and available cache objects are valid
```

Checks that the Git worktree exists, that every pointer parses and validates,
that every cached directory manifest parses, and that every *present* cache
object re-hashes to the size and digest its pointer or manifest entry claims. Objects that are absent from the cache are skipped, not reported as
errors — use `avc status` to find those.

Fails on the first corrupt object with `corrupt cache object for <path>`.

---

## Exit codes

[`SPEC.md`](../SPEC.md) reserves four codes:

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Expected user, data, or state error |
| `2` | Invalid CLI usage |
| `3` | Provider or operational failure |

**Current implementation:** all four are emitted. `2` comes from argument
parsing. `3` covers provider and operational failures — an unreachable endpoint,
a rejected signature, missing credentials, or a provider with no adapter. `1`
covers everything else, including a remote object that fails to match its
pointer, which is a data error rather than an infrastructure one.

```bash
avc push; case $? in
  0) echo "pushed" ;;
  1) echo "repository or data problem" ;;
  3) echo "storage unreachable — safe to retry" ;;
esac
```

Runtime errors are written to stderr as `avc: <message>`.

## Output format stability

Command output is human-oriented and **not a stable interface** in `0.1.0`.
Tab-separated `status` and `list` output is convenient for `awk`, but it may
change. A machine-readable `--format json` is on the [Roadmap](roadmap.md); if
you need it, that issue is a good place to start contributing.
