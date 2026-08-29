# CLI Reference

Everything `avc` can do in `0.1.0`. Behavior described here reflects the current
implementation, including its gaps — where a flag is accepted but not yet
honored, this page says so.

```text
avc init
avc remote add <name> <provider-url>
avc remote list
avc add <path> [<path>...]
avc list [<path>...] [--repo <git-url>] [--ref <rev>] [--remote <name>] [--porcelain]
avc status [--porcelain]
avc commit <path> [<path>...] [--force]
avc push [<path>...] [--remote <name>]
avc pull [<path>...] [--remote <name>]
avc checkout [<path>...] [--force]
avc remove <path> [<path>...]
avc gc [--remote <name>] [--dry-run]
avc doctor

# built for CI/CD -- see docs/ci-cd.md
avc fetch [<path>...] [--repo <git-url>] [--ref <rev>]
          [--remote <name>] [--remote-url <url>]
          [--output <dir>] [--cache <dir>] [--force] [--dry-run] [--porcelain]
avc verify [<path>...] [--repo <git-url>] [--ref <rev>]
           [--output <dir>] [--porcelain]
```

Every command also accepts `--color <auto|always|never>` and
`--progress <auto|always|never>`.

## Global behavior

**Repository discovery.** Every command except `fetch`, `verify`, `--help`, and
`--version` walks up from the current directory until it finds a `.git` entry. That directory is the
repository root, and all artifact paths are relative to it. If no `.git` is
found, the command fails with `not inside a Git worktree`.

**Initialization check.** Every command except `init`, `fetch`, and `verify`
requires `.avc/config.toml` to exist, otherwise:
`AVC is not initialized; run 'avc init'`.

**Repository-free commands.** `avc fetch`, `avc verify`, and `avc list` need
neither when given `--repo <git-url>`: they read the pointers, and the object
store the pointers belong to, out of a shallow read of one revision of that
repository. That is what lets a build agent name a repository and a path inside
it without cloning anything. See [CI/CD](ci-cd.md).

**Revisions.** Those same three commands take `--ref <rev>` (or `$AVC_REF`),
which names the revision of the registry to read pointers at. Anything that
names one commit works: a branch, a tag, `HEAD` for the default branch, a commit
id whole or abbreviated, or a fully qualified `refs/heads/…` or `refs/tags/…`
name for a repository where a branch and a tag collide. The commit it resolved
to is printed in the heading, so a log records what a moving branch meant on the
day it ran.

`--ref` works with or without `--repo`. Inside a checkout it reads the pointers
Git holds at that commit rather than the ones on disk, which is what makes
`avc verify --ref v1.0.0` a real question about the working tree. The artifacts
still belong to the worktree, so `avc fetch --ref v1.0.0 --force` restores that
version in place.

Omitting `--ref` is not the same as `--ref HEAD`. With no revision, a local
repository is read off the working tree — an uncommitted pointer still counts,
as everywhere else — while naming a revision always reads Git.

An abbreviated commit id costs more than the rest: a prefix is not a name, so no
server can look one up, and AVC falls back to fetching every branch and tag
(commits and trees, not file contents) to resolve it locally. Name a tag, a
branch, or a whole commit id in a pipeline.

**Pointer discovery.** Commands that operate on "all artifacts" find them by
recursively scanning the worktree for files ending in `.avc`, skipping the `.git`
and `target` directories. Pointers are found on disk, not read out of Git's
index, so a pointer you have not committed still counts.

**Path validation.** Every path is normalized and validated: it must be
repository-relative, with no `..`, no `.`, no absolute prefix, and no backslash.

**Path selection.** Every command that takes artifact paths — `commit`, `push`,
`pull`, `checkout`, `remove`, `fetch`, `verify`, `list` — uses one rule. A path
matches an artifact exactly, or acts as a directory prefix naming every artifact
beneath it; naming nothing selects everything. An exact match always wins over a
prefix, a trailing `/` is optional, and a trailing `.avc` is stripped, so a
pointer path from `git diff --name-only` names its artifact unchanged. A path
that matches nothing is an error rather than an empty selection.

**Ordering.** Commands that operate on all artifacts process them in sorted path
order, so repeated runs — and runs on different machines — print the same
sequence.

**Color.** Output is colorized when stdout is a terminal that wants it. The
`--color <auto|always|never>` flag, the `AVC_COLOR` environment variable,
`NO_COLOR`, `CLICOLOR_FORCE`, and `TERM=dumb` are all honored, in that order of
precedence. Color is decoration: every line reads identically without it.

**Progress.** `push`, `pull`, and `fetch` report how far along a transfer is,
in one of two forms. At a terminal it is a bar on **stderr** that redraws in
place and erases itself when the command finishes, so it never lands in a
redirect and never scrolls the artifact lines away:

```text
uploading [=============>          ]  60%  24.5 MiB/41.0 MiB  models/bert/weights.bin  43.2 MiB/s  eta 0:11
```

In a CI pipeline it is an ordinary line on stdout every ten seconds instead,
because a log is a file read after the fact and an animated line is thousands of
unreadable fragments in it:

```text
uploading     60%  1/4 objects  24.5 MiB/41.0 MiB (43.2 MiB/s, eta 0:11)
```

A pipeline is recognized by `CI`, `CONTINUOUS_INTEGRATION`, `GITHUB_ACTIONS`,
`GITLAB_CI`, `JENKINS_URL`, `TEAMCITY_VERSION`, or `TF_BUILD` being set to
anything other than `0`, `false`, or `no`; that test wins over the terminal
check, since some runners do allocate a pseudo-terminal. A pipe, a redirect, and
`TERM=dumb` get the same lines a pipeline does. `--progress <auto|always|never>`
and `$AVC_PROGRESS` override the choice, `--porcelain` suppresses it entirely,
and the bar is laid out for `$COLUMNS` columns, or 80 when that is unset.
Everything progress reports is also in the summary each command prints when it
finishes, so a run with progress turned off loses nothing.

**Porcelain.** `status`, `list`, `fetch`, and `verify` accept `--porcelain`,
which prints tab-separated records with no header, no summary, and no color.
That format is a stable interface; the human-facing tables are not. See
[Output format stability](#output-format-stability).

---

## `avc init`

Initialize AVC in the current Git worktree.

```bash
avc init
```

```text
initialized AVC in /path/to/repo
  config     .avc/config.toml
  cache      .avc/cache
  ignored    .avc/cache/, .avc/config.local.toml

next: avc remote add origin <url>, then avc add <path>
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

```text
configured remote origin
  provider   s3
  location   my-bucket/artifacts
  default    yes
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
   NAME    PROVIDER  LOCATION
*  origin  file      /tmp/avc-remote
   backup  s3        my-bucket/artifacts

* marks the remote used when --remote is omitted
```

`LOCATION` is `bucket/prefix`; it never contains credentials.

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
tracked      model.bin (17 B, 1dfc4d103921)
```

The digest is shown as its first twelve characters; the pointer file holds all
sixty-four.

Fails if the path is neither a regular file nor a directory.

Re-running `add` on a changed file updates the pointer to the new content and
adds a new cache object. The previous object remains until `gc`.

### Directories

A directory is one artifact with one pointer, the way `dvc add` treats one:

```bash
avc add data/
```

```text
tracked      data/ (3 files, 17 B, bb292fab8a18)
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
avc status [--porcelain]
```

```text
STATUS    CACHE            SIZE  ARTIFACT
modified  cached        4.0 MiB  data/train.parquet
ok        cached           17 B  model.bin
missing   cache-missing       -  weights/final.safetensors

3 artifacts: 1 ok, 1 modified, 1 missing
```

`SIZE` is what is on disk now, not what the pointer claims — for a `modified`
artifact those differ, which is often the useful part. A `missing` artifact
shows `-`.

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

Unparseable pointers are collected and listed under `invalid pointers:` after
the table, rather than aborting the run — one bad pointer must not hide the
state of every other artifact.

Prints `no AVC pointers found` when nothing is tracked.

`--porcelain` prints `<state>\t<cache>\t<path>`, one line per artifact, with no
header or summary. An unparseable pointer becomes
`invalid\t-\t<path>: <error>`.

**Cost note:** `status` re-hashes every existing artifact. On a repository with
hundreds of gigabytes tracked, this is I/O-bound and slow. There is no
mtime/size fast path yet — see [Roadmap](roadmap.md).

---

## `avc list`

Browse what a repository stores, **without downloading artifact bytes**.

```bash
avc list [<path>...] [--repo <git-url>] [--ref <rev>]
         [--remote <name>] [--remote-url <url>] [--porcelain]
```

```text
everything in /home/me/artifacts
  objects    https://s3.eu-west-1.amazonaws.com/my-bucket

PATH                     SIZE  OBJECT        REMOTE
data/                 4.0 MiB  9a3b1c77e004  missing
model.bin                17 B  1dfc4d103921  available

2 artifacts, 4.0 MiB: 1 available, 1 missing
```

Uses the repository on disk unless `--repo <git-url>` names one, and the object
store the repository configures unless `--remote-url` overrides it.

### Listing a path

A path argument scopes the listing, which is what makes a repository holding a
hundred artifacts browsable rather than a wall of rows.

| Argument | Lists |
| --- | --- |
| *(none)* | every artifact in the repository |
| `models/bert` | the artifacts beneath that prefix |
| `models/bert/weights.bin` | that one artifact |
| `data` — a **tracked directory** | the files stored inside it |

The last case is the interesting one. A directory artifact is one row in the
full listing, because it is one artifact; naming it exactly asks to look inside:

```bash
avc list data
```

```text
PATH                      SIZE  OBJECT        REMOTE
data/raw/2024-01.csv   1.1 GiB  87428fc52280  available
data/raw/2024-02.csv   1.2 GiB  0263829989b6  available

2 files, 2.3 GiB: 2 available, 0 missing
```

Those rows come from the directory's manifest, which `list` fetches from the
remote when it is not already cached. A manifest is metadata of a few bytes per
file; artifact bytes are still never downloaded. If the manifest is on neither
side, the contents are unknowable and the command says so rather than guessing.

### Availability

Availability is resolved with a single prefixed listing of the remote, not one
request per artifact, so a repository with a thousand pointers costs one round
trip. Objects in the bucket that AVC did not write are ignored.

For a directory row, `SIZE` is the total bytes of the files it contains, not the
size of its manifest, and `REMOTE` reads `available` only when the manifest *and*
every file it names are on the remote — a half-uploaded directory cannot be
restored. When the manifest is on neither side, the row reads `-` and `missing`.

`--porcelain` prints `<path>\t<bytes>\t<algorithm:full-hash>\t<remote-state>`
with no heading, table, or summary.

> **Limitation:** `gs://` and `az://` remotes fail with
> `provider adapter not implemented`.

---

## `avc push`

Upload cached objects to a remote.

```bash
avc push [<path>...] [--remote <name>]
```

With no paths, pushes every tracked artifact; otherwise the paths given, using
the shared [path selection](#global-behavior) rule — so `avc push models/bert`
pushes everything beneath that prefix, and `avc push data` pushes the one
directory artifact named `data`.

Fails with `cache object missing for <path>` if the object is not in the local
cache — run `add`/`commit` or `pull` first. Naming a path that has no pointer is
an error rather than a silent no-op.

```text
pushing 2 artifacts to https://s3.eu-west-1.amazonaws.com/my-bucket

uploaded     data/ (4 objects, 12.0 MiB)
up-to-date   model.bin

pushed 4 objects (12.0 MiB) to https://s3.eu-west-1.amazonaws.com/my-bucket
```

Objects already present on the remote are skipped — reported as
`up-to-date` — because content-addressed objects are immutable and
re-uploading identical bytes is pure cost. Uploads stream in bounded memory,
with [progress](#global-behavior) reported as they go.

The remote is asked what it already holds for every selected object before the
first byte is uploaded, which is what gives progress a total to measure against.
It is the same set of `HEAD` requests either way, so it costs nothing extra.

A directory uploads as its files followed by its manifest — `uploaded data/ (4
objects, 12.0 MiB)` — in that order, so a manifest on the remote never names bytes that
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

```text
pulling 2 artifacts from https://s3.eu-west-1.amazonaws.com/my-bucket

downloaded   data/ (4 objects, 12.0 MiB)
up-to-date   model.bin

checked out  data/ (3 files)
checked out  model.bin (17 B)

pulled 4 objects (12.0 MiB) from https://s3.eu-west-1.amazonaws.com/my-bucket
```

`pull` is `download` followed by an implicit `checkout` **without** `--force`, so
it will refuse to overwrite a locally modified file. Fetch the bytes and resolve
the conflict deliberately with `avc checkout --force` if that is what you want.

Fails with `remote object not found: <hash>` if the remote lacks the object.
Objects already in the cache are not re-downloaded.

A directory downloads its manifest first — until that has arrived and been
verified, the rest of its objects are unknown — then every file the manifest
names, then materializes the tree. That is also the one case where the
[progress](#global-behavior) total grows part way through: on a first pull, the
files a directory contains are counted the moment its manifest names them.

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
untracked    model.bin
note: the artifact and its cached bytes are kept; reclaim them with `avc gc`
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

```text
removable    9a3b1c77e004
removable    bb292fab8a18

reclaimable: 2 objects (392 B)
note: re-run without --dry-run to delete them
```

Without `--dry-run` the verb becomes `removed` and the summary `reclaimed`. When
every cache object is still referenced it prints
`nothing to reclaim: every cache object is still referenced`.

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

re-hashed 6 cache objects named by 3 pointers
```

Checks that the Git worktree exists, that every pointer parses and validates,
that every cached directory manifest parses, and that every *present* cache
object re-hashes to the size and digest its pointer or manifest entry claims. Objects that are absent from the cache are skipped, not reported as
errors — use `avc status` to find those.

Fails on the first corrupt object with `corrupt cache object for <path>`.

---

## CI/CD commands

`avc fetch` and `avc verify` are built for a build agent rather than a
workstation. Given `--repo <git-url>`, they read the pointers — and the object
store those pointers belong to — out of a shallow read of one Git reference, so a
job names a repository and a path inside it and never a bucket. `avc list`,
documented [above](#avc-list), takes the same two arguments.

The [CI/CD guide](ci-cd.md) covers credentials, caching between jobs,
least-privilege policies, and worked pipelines; this section is the flag
reference.

### `avc fetch`

Download the artifacts at a path in a repository, straight to where they belong.

```bash
avc fetch [<path>...] [--repo <git-url>] [--ref <rev>]
          [--remote <name>] [--remote-url <url>]
          [--output <dir>] [--cache <dir>]
          [--force] [--dry-run] [--porcelain]
```

```text
fetching 2 artifacts from https://github.com/acme/artifacts@a4f21c0be931 (HEAD)
  objects    https://s3.eu-west-1.amazonaws.com/acme-artifacts
  into       .

downloaded   models/bert/tokenizer.json (3 B)
downloaded   models/bert/weights.bin (878.9 MiB)

fetched 2 objects (878.9 MiB) for 2 artifacts
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `<path>...` | every artifact | Paths inside the repository — an artifact, a prefix, or `-` for stdin |
| `--repo <url>` | `$AVC_REPO`, else the checkout on disk | Git URL to read pointers from |
| `--ref <rev>` | `$AVC_REF`, else the default branch, or the pointers on disk | Revision to read pointers at: a branch, tag, commit, or `refs/...` name |
| `--remote <name>` | the repository's default | Named object store, when it configures several |
| `--remote-url <url>` | the repository's own | Object store URL, overriding what the repository says |
| `-o`, `--output <dir>` | worktree root, or `.` with `--repo` | Root the pointers' paths are resolved against |
| `--cache <dir>` | `$AVC_CACHE_DIR`, else none | Read from and populate a cache directory |
| `--force` | off | Overwrite files whose contents differ from their pointer |
| `--dry-run` | off | Report the transfer without making it |
| `--porcelain` | off | `<state>\t<objects>\t<bytes>\t<path>`, no heading or summary |

**Where the object store comes from.** The repository's tracked
`.avc/config.toml`, read at the same reference as the pointers. A consumer does
not name a bucket, a prefix, or an endpoint, and a repository that moves its
storage does not break them. `--remote-url` overrides it for one run — a mirror,
or an air-gapped copy — and `--remote` picks between several the repository
already configures.

**Reading from a Git URL** shells out to `git`, which must be on `PATH`. The
checkout is one commit deep, holds only text — artifacts are gitignored — and
lives in a temporary directory deleted when the command ends. The commit a
reference resolved to is printed, so a log records what a moving branch was.

**What it writes.** Only artifacts. No clone in your workspace, no cache unless
`--cache` is given, no `.avc/` directory, no `.gitignore` edits, and a directory
artifact's manifest is never written into the output tree.

**Object states.** `downloaded` (bytes came over the network), `from-cache`
(served from `--cache`, or from an identical object already fetched this run),
`up-to-date` (already on disk and already correct), `would-fetch` (a dry run).

**Verification.** Each object is hashed as it is written and checked against its
pointer's size and digest before it becomes visible; a mismatch leaves no partial
file behind. A directory's manifest is verified before it is parsed, because it
decides where `fetch` writes.

**Idempotence.** A file already hashing to what its pointer claims is left alone.
A file that differs is a refusal — `refusing to replace <path>: it differs from
its pointer; use --force` — not an overwrite.

**Deduplication.** Identical objects transfer once per run even with no cache:
the second is copied from wherever the first landed. The reported object and byte
counts are artifact content only; reading a directory's manifest is metadata and
is not counted.

> **Limitations:** a path *inside* a tracked directory cannot be named — a
> directory artifact is fetched whole. No resumable download: a dropped
> connection restarts that object, though objects already written are not
> re-fetched, so re-running resumes at object granularity. `gs://` and `az://`
> fail with `provider adapter not implemented`.

### `avc verify`

Check artifacts on disk against their pointers, using nothing but the two.

```bash
avc verify [<path>...] [--repo <git-url>] [--ref <rev>]
           [--output <dir>] [--porcelain]
```

```text
verifying 3 artifacts against https://github.com/acme/artifacts@a4f21c0be931 (HEAD)
  in         .

STATUS        SIZE  ARTIFACT
ok             3 B  models/bert/tokenizer.json
ok       878.9 MiB  models/bert/weights.bin
missing          -  models/gpt/weights.bin

3 artifacts checked: 2 ok, 1 not matching
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `<path>...` | every artifact | Same path selection as `avc fetch` |
| `--repo <url>` | `$AVC_REPO`, else the checkout on disk | Git URL to read pointers from |
| `--ref <rev>` | `$AVC_REF`, else the default branch, or the pointers on disk | Revision to read pointers at: a branch, tag, commit, or `refs/...` name |
| `-o`, `--output <dir>` | worktree root, or `.` with `--repo` | Root the artifacts were written into |
| `--porcelain` | off | `<status>\t<bytes on disk>\t<path>`, no heading or summary |

No object store is contacted and no credentials are read. Exits `1` if any
artifact is `modified` or `missing`, which is what makes it usable as a pipeline
gate — including against a released tag, which answers whether a deployed
directory still matches a particular commit.

Finding no artifacts is **not** a failure: it prints `no AVC pointers found` and
exits `0`, matching `avc status`. A gate that must not pass on an empty selection
should name its paths explicitly.

For a directory, `modified` covers a file edited, added, removed, or renamed
anywhere beneath it, because the directory's identity is the hash of the manifest
of its contents.

This is `avc status` minus the repository and minus the cache column. Use
`status` in a checkout; use `verify` in a job, or against a Git reference.

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
a rejected signature, missing credentials, a provider with no adapter, and (for
`--repo`) a Git reference that does not exist or a missing `git` command. `1`
covers everything else, including a path that names no artifact and a remote
object that fails to match its pointer, which are data errors rather than
infrastructure ones.

```bash
avc push; case $? in
  0) echo "pushed" ;;
  1) echo "repository or data problem" ;;
  3) echo "storage unreachable — safe to retry" ;;
esac
```

See [CI/CD](ci-cd.md#exit-codes) for how a pipeline should react to each.

Runtime errors are written to stderr as `avc: <message>`.

## Output format stability

The tables, summaries, and colors are human-oriented and **not a stable
interface**. They are expected to change.

`--porcelain`, on `status`, `list`, `fetch`, and `verify`, is the interface to
script against: tab-separated records, one per artifact, no header, no summary,
no color, and a stable column order documented with each command. Anything a
pipeline parses should go through it.

A richer `--format json` is on the [Roadmap](roadmap.md); if you need it, that
issue is a good place to start contributing.
