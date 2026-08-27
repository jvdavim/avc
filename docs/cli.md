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

Track one or more files.

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

Fails if the path is not a regular file. **Directories are rejected** — pass
individual files.

Re-running `add` on a changed file updates the pointer to the new content and
adds a new cache object. The previous object remains until `gc`.

## `avc commit`

Record a new version of an **already-tracked** artifact.

```bash
avc commit <path> [<path>...] [--force]
```

Identical to `add`, except it first requires that a pointer already exists for
the path, failing with `no pointer exists for <path>` if not.

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
| `cached` | Referenced object is present in `.avc/cache` |
| `cache-missing` | Not present; `pull` before `checkout` |

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

> Path filtering here matches the *pointer file path*, so `avc checkout model.bin`
> selects `model.bin.avc`.

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

Reachability is computed from pointer files **in the current working tree only**.

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

Checks that the Git worktree exists, that every pointer parses and validates, and
that every *present* cache object re-hashes to the size and digest its pointer
claims. Objects that are absent from the cache are skipped, not reported as
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
