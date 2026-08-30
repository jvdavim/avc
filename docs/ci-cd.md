# AVC in CI/CD

How to get artifacts into a build, and how to prove you got the right ones.

An AVC repository is an **artifact registry**. One Git repository can hold the
models, datasets, and archives of a dozen projects, and a job that needs one of
them should pay for one of them. So the commands below are given a *repository*
and a *path inside it* — never a bucket, and never the whole thing unless that
is what you asked for.

| Command | What it does |
| --- | --- |
| [`avc fetch`](#avc-fetch) | Downloads the artifacts at a path |
| [`avc list`](#avc-list) | Shows what is stored at a path |
| [`avc verify`](#avc-verify) | Checks artifacts on disk against their pointers |

None of them needs a clone, an `avc init`, or a local cache.

## The 30-second version

```yaml
- run: avc fetch --repo https://github.com/acme/artifacts models/bert --output .
  env:
    AWS_ACCESS_KEY_ID: ${{ secrets.AWS_ACCESS_KEY_ID }}
    AWS_SECRET_ACCESS_KEY: ${{ secrets.AWS_SECRET_ACCESS_KEY }}
```

```text
fetching 2 artifacts from https://github.com/acme/artifacts@a4f21c0be931 (HEAD)
  objects    https://s3.eu-west-1.amazonaws.com/acme-artifacts
  into       .

downloaded   models/bert/tokenizer.json (3 B)
downloaded   models/bert/weights.bin (878.9 MiB)

fetched 2 objects (878.9 MiB) for 2 artifacts
```

Notice what is *not* in that command: a bucket, a prefix, an endpoint, or a list
of files. AVC reads the pointers at that Git reference, learns the object store
from the repository's own `.avc/config.toml`, and downloads exactly the objects
the pointers under `models/bert` name — verifying each as it streams, straight to
the path the pointer says.

---

## The two halves of a repository

An AVC repository lives in two places:

| Half | Holds | Who sets it up |
| --- | --- | --- |
| **Git** | Pointer files, and `.avc/config.toml` | Committed like any other file |
| **Object store** | The artifact bytes | Once, with `avc remote add` |

A pointer says *which object*; the configuration says *which store*. Both travel
together in the same commit, so a consumer only ever needs the first address:

```bash
avc fetch --repo https://github.com/acme/artifacts models/bert -o .
```

This is why credentials, not coordinates, are the only thing a pipeline
configures. Move the bucket, change the endpoint, migrate provider — consumers do
not change, because they never named it. If you do need to override it for one
run (a mirror, an air-gapped copy), `--remote-url` still takes an object-store
URL directly.

The Git half is cheap to read. Artifacts are gitignored, so a shallow one-commit
checkout of an artifact registry is its pointer files and its configuration —
kilobytes of text — read into a temporary directory that is deleted when the
command ends. Nothing is written to your workspace but the artifacts you asked
for.

> Reading from `--repo` shells out to `git`, so the `git` command must be on
> `PATH`. Everything else in AVC works without it.

---

## Why not `avc pull`?

`avc pull` is built for a workstation. In a pipeline it is the wrong shape:

| | `avc pull` | `avc fetch` |
| --- | --- | --- |
| Needs a clone | yes, the whole repository | no, one shallow ref |
| Needs `avc init` | yes | no |
| Object store | from the checkout's config | from the repository's config |
| Selecting artifacts | paths in *your* checkout | paths in the registry |
| Local cache | always populated | none by default, `--cache` optional |
| Disk used | 2x the artifact size | 1x |
| S3 permissions | `GetObject`, plus `ListBucket` for `list` | `GetObject` only |

The disk difference is not academic. `pull` writes every object into
`.avc/cache` and then copies it into the worktree, so a job pulling 40 GB of
checkpoints needs 80 GB of runner disk — and then throws the cache away when the
job ends. `fetch` streams from the store to the destination path, hashing as it
writes, so a 40 GB artifact costs 40 GB and a few megabytes of RSS.

Use `pull` when the job really is a developer's checkout and you want the cache.
Use `fetch` everywhere else.

---

## `avc fetch`

```text
avc fetch [<path>...] [--repo <git-url>] [--ref <ref>]
          [--remote <name>] [--remote-url <url>]
          [--output <dir>] [--cache <dir>]
          [--force] [--dry-run] [--porcelain]
```

### Naming the repository

| Source | Example | Needs a checkout |
| --- | --- | --- |
| `--repo` | `--repo https://github.com/acme/artifacts` | no |
| `$AVC_REPO` | `AVC_REPO=https://github.com/acme/artifacts` | no |
| *(neither)* | reads the pointers already on disk | yes |

### Naming the revision

`--ref` (or `$AVC_REF`) selects which revision of the registry to read pointers
at. Anything that names one commit works:

| Revision | Example | Cost |
| --- | --- | --- |
| Default branch | *(omitted)*, or `HEAD` | one shallow fetch |
| Branch | `--ref main` | one shallow fetch |
| Tag | `--ref v2.1.0` | one shallow fetch |
| Commit | `--ref a4f21c0be9315d0f2c8e...` | one shallow fetch |
| Abbreviated commit | `--ref a4f21c0` | see below |
| Fully qualified | `--ref refs/tags/v2.1.0` | one shallow fetch |

Pin a tag or a commit for anything reproducible:

```bash
avc fetch --repo https://github.com/acme/artifacts --ref v2.1.0 models/bert -o .
```

The commit a revision resolved to is printed in the heading — `@a4f21c0be931`
above — so a log records exactly which version of a moving branch a build used.

Reach for `refs/tags/…` or `refs/heads/…` only when a branch and a tag share a
name; otherwise the short form means the same thing.

**Abbreviated commits cost more.** A prefix is not a name, so no server can look
one up: AVC falls back to fetching every branch and tag — commits and trees, but
not file contents — and resolving the prefix locally. That is fine at a
workstation and wasteful in a pipeline, where `${{ github.sha }}` and its
equivalents are already whole. Name a tag, a branch, or a full commit in CI.

Any URL `git` understands works, including `git@host:org/repo.git` and
`file:///path/to/repo`.

### A revision inside a checkout

`--ref` works without `--repo` too, against the repository you are standing in.
It reads the pointers Git holds at that commit rather than the ones on disk, so
it answers "what was this artifact at `v1.0.0`?" without moving your branch:

```bash
avc list   --ref v1.0.0            # what that release shipped
avc verify --ref v1.0.0            # does the working tree still match it?
avc fetch  --ref v1.0.0 --force    # put that version back
```

The artifacts belong to your worktree, not to the temporary checkout the
pointers came out of, so `fetch` writes them where they normally live — and, as
always, refuses to overwrite anything that differs until told to with `--force`.

Omitting `--ref` is not the same as `--ref HEAD`. With no revision, a local
repository is read off the working tree, so a pointer you have written but not
committed still counts, exactly as it does for `status` or `push`. Naming a
revision always reads Git.

### Naming the path

Positional arguments are paths **inside the repository**:

| Argument | Selects |
| --- | --- |
| *(none)* | every artifact in the repository |
| `models/bert/weights.bin` | that one artifact |
| `models/bert` | every artifact beneath it |
| `models/bert/weights.bin.avc` | the same artifact; the `.avc` is stripped |
| `data` (a tracked directory) | that directory artifact, whole |
| `-` | newline-separated paths read from stdin |

A trailing `/` is optional everywhere. An exact match always wins over a prefix,
so a directory artifact named `data` is one artifact rather than a prefix over
anything that happens to start with those letters.

This is the same path language `avc push`, `avc pull`, and `avc checkout` use in
a checkout, so `avc push models/bert` and `avc fetch models/bert` mean the same
thing on either side of the pipeline.

Stdin lets a job select with the tools it already has:

```bash
# only the artifacts this commit changed
git diff --name-only HEAD~1 -- '*.avc' | avc fetch -
```

Naming a path that matches nothing is an error, not an empty selection — a typo
in a pipeline should fail the job rather than silently fetch nothing.

> **Not yet supported:** naming a path *inside* a tracked directory. `avc fetch
> data/raw` works only if `data/raw` is itself a tracked artifact; a directory
> artifact is fetched whole. See the [roadmap](roadmap.md).

### Where files land

`--output` (`-o`) is the root the pointers' paths are resolved against. A pointer
for `models/bert/weights.bin` fetched with `-o /srv/app` lands at
`/srv/app/models/bert/weights.bin`; parent directories are created as needed. The
layout inside the repository is preserved, which is what makes a fetch into a
checkout land exactly where `avc pull` would have put it.

With `--repo`, `--output` defaults to the current directory. Without it — when
pointers come from a checkout — it defaults to that repository's root, so a
command run from a subdirectory still puts artifacts where their paths say.

A directory artifact materializes as its files and nothing else; the manifest
naming them stays in AVC's bookkeeping and is never written into the output.

### Re-running a job

`fetch` is idempotent and cheap to repeat. Before transferring anything it checks
what is already on disk:

```text
up-to-date   models/bert/weights.bin (878.9 MiB)
```

A file whose contents already hash to what the pointer claims is left alone and
costs one local read instead of a download. That makes `fetch` safe in a step
that reruns, and makes a retried job skip everything the first attempt finished.

A file that exists but **differs** is a refusal, not an overwrite:

```text
avc: refusing to replace data/a.bin: it differs from its pointer; use --force
```

A fresh workspace never hits this. A reused runner workspace might, and `--force`
is the answer there — the same rule, and the same escape hatch, that `avc
checkout` uses.

### Caching between jobs

`--cache <dir>` (or `$AVC_CACHE_DIR`) makes `fetch` read from and write to a
content-addressed cache directory a runner can persist between jobs:

```yaml
- uses: actions/cache@v4
  with:
    path: .avc-cache
    key: avc-${{ hashFiles('**/*.avc') }}
- run: avc fetch --cache .avc-cache --output .
```

Cache entries are verified by re-hashing before use, and one that fails is
deleted and re-downloaded. The key above is content-addressed by construction:
when no pointer changed, nothing was fetched.

This costs disk — the cache holds a second copy of every object — so it is worth
it when the artifacts are large *and* the runner's cache is faster than your
object store. Without `--cache`, nothing is duplicated.

### Output

| Word | Meaning |
| --- | --- |
| `downloaded` | Bytes came over the network |
| `from-cache` | Served from `--cache`, or from an identical object already fetched this run |
| `up-to-date` | Already on disk and already correct; nothing was written |
| `would-fetch` | `--dry-run`: this is what a real run would transfer |

The object and byte counts are artifact content only. Reading a directory's
manifest is not counted: it is a few bytes of metadata `fetch` must read to know
what else to ask for, and counting it would make a directory that is entirely up
to date report a download on every run.

Identical files are transferred once even with no cache, whether they are two
paths inside one directory artifact or two separate artifacts.

### Machine-readable output

`--porcelain` prints one tab-separated line per artifact and nothing else:

```text
downloaded	1	921174016	models/bert/weights.bin
up-to-date	0	0	models/bert/tokenizer.json
```

| Column | Value |
| --- | --- |
| 1 | `downloaded`, `from-cache`, `up-to-date`, or `would-fetch` |
| 2 | objects transferred |
| 3 | bytes transferred |
| 4 | artifact path, with a trailing `/` for a directory |

Unlike the human output above, this format is stable — script against it rather
than against the table.

### Dry runs

`--dry-run` reports exactly what a real run would transfer and writes nothing:

```bash
avc fetch --repo "$AVC_REPO" models --dry-run --porcelain \
  | awk -F'\t' '{ bytes += $3 } END { print bytes }'
```

It still reads directory manifests, because that is the only way to know what a
directory contains. It never reads or writes artifact bytes, and never touches
the cache.

---

## `avc list`

```text
avc list [<path>...] [--repo <git-url>] [--ref <ref>]
         [--remote <name>] [--remote-url <url>] [--porcelain]
```

Browsing a registry, without downloading anything. Availability is resolved with
a single listing of the object store, so a repository with a thousand artifacts
costs one round trip.

**With no path**, every artifact in the repository. A tracked directory is one
row, because it is one artifact:

```text
everything in https://github.com/acme/artifacts@a4f21c0be931 (HEAD)
  objects    https://s3.eu-west-1.amazonaws.com/acme-artifacts

PATH                             SIZE  OBJECT        REMOTE
data/                         4.2 GiB  e59967c656df  available
models/bert/tokenizer.json        3 B  ca3d163bab05  available
models/bert/weights.bin     878.9 MiB  90e38fb2627b  available
models/gpt/weights.bin      390.6 MiB  348ce6621a96  missing

4 artifacts, 5.4 GiB: 3 available, 1 missing
```

**With a prefix**, just that corner of it — which is how you find out what a
project owns without reading the whole registry:

```bash
avc list --repo https://github.com/acme/artifacts models/bert
```

**With a tracked directory named exactly**, the files stored inside it:

```bash
avc list --repo https://github.com/acme/artifacts data
```

```text
PATH                      SIZE  OBJECT        REMOTE
data/raw/2024-01.csv   1.1 GiB  87428fc52280  available
data/raw/2024-02.csv   1.2 GiB  0263829989b6  available
data/raw/2024-03.csv   1.9 GiB  55a54008ad1b  available

3 files, 4.2 GiB: 3 available, 0 missing
```

Reading that list needs the directory's manifest, which `list` fetches when it is
not already local. A manifest is a few bytes per file; artifact bytes are still
never downloaded.

`REMOTE` reads `available` only when every object the row needs is on the store —
for a directory, its manifest *and* every file it names, since a half-uploaded
directory cannot be restored.

`--porcelain` prints `<path>\t<bytes>\t<algorithm:full-hash>\t<remote-state>`
with no heading, table, or summary.

---

## `avc verify`

```text
avc verify [<path>...] [--repo <git-url>] [--ref <ref>]
           [--output <dir>] [--porcelain]
```

Re-hashes what is on disk and compares it with what the pointers claim. No object
store is contacted and no credentials are read — the pointers and the bytes are
everything it needs. It exits `1` if anything is missing or differs, which makes
it a gate.

```text
verifying 3 artifacts against https://github.com/acme/artifacts@a4f21c0be931 (HEAD)
  in         .

STATUS        SIZE  ARTIFACT
ok             3 B  models/bert/tokenizer.json
ok       878.9 MiB  models/bert/weights.bin
missing          -  models/gpt/weights.bin

3 artifacts checked: 2 ok, 1 not matching
```

| Status | Meaning |
| --- | --- |
| `ok` | Present, and its bytes hash to what the pointer says |
| `modified` | Present, and its bytes do not |
| `missing` | Not there at all |

For a directory, `modified` covers a file edited, added, removed, or renamed
anywhere beneath it: the directory's identity is the hash of the manifest of its
contents, so any of those changes it.

Because it takes `--repo`, it answers a question a checksum file cannot: *does
this deployed directory still match commit `a4f21c0` of the registry?*

```bash
avc verify --repo https://github.com/acme/artifacts --ref v2.1.0 models -o /srv/app
```

Useful places for it:

- **After a fetch into a workspace you do not control**, before you trust it.
- **After a build**, to prove nothing overwrote an input.
- **At the start of a job that inherited a workspace** from an earlier stage.
- **On a running host**, to detect drift against a released tag.

`--porcelain` prints `<status>\t<bytes on disk>\t<path>`.

> Finding no artifacts is not a failure: it prints `no AVC pointers found` and
> exits `0`. If a gate must not pass on an empty selection, name the paths
> explicitly rather than relying on the default of "everything".

---

## Credentials

Two different things may need authenticating, and they are unrelated:

| For | Uses |
| --- | --- |
| Reading pointers from `--repo` | Whatever `git` is configured with — an SSH key, a token in the URL, a credential helper |
| Reading bytes from the object store | The environment variables below |

AVC reads provider-standard variables first, which is where every CI system puts
secrets. Nothing needs to be written to disk.

| Variable | Purpose |
| --- | --- |
| `AWS_ACCESS_KEY_ID` | Required for S3 remotes |
| `AWS_SECRET_ACCESS_KEY` | Required for S3 remotes |
| `AWS_SESSION_TOKEN` | Temporary credentials — set this when using OIDC or `assume-role` |
| `AWS_REGION` / `AWS_DEFAULT_REGION` | Signing region; defaults to `us-east-1` |
| `AWS_ENDPOINT_URL_S3` / `AWS_ENDPOINT_URL` | Overrides the endpoint, for MinIO, R2, Ceph |
| `AVC_CA_BUNDLE` / `AWS_CA_BUNDLE` / `SSL_CERT_FILE` | PEM bundle of certificate authorities to trust, for a runner behind a TLS-inspecting proxy |
| `AVC_SYSTEM_CERTS` | `1` to verify against the runner's own trust store instead of the built-in roots |

> **Not supported yet:** AVC does not call instance-metadata endpoints, so IAM
> instance roles, ECS task roles, and SSO do not work on their own. Federated
> credentials still work as long as something exchanges them for the three
> variables above first — which is exactly what
> `aws-actions/configure-aws-credentials` and `assume-role` wrappers do.

A token in a `--repo` URL is never echoed: AVC redacts `user:password@` from any
URL it prints, including inside Git's own error messages.

### Least privilege

`avc fetch` issues `GetObject` requests and nothing else. A fetch-only job should
have a policy that says so:

```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Action": "s3:GetObject",
    "Resource": "arn:aws:s3:::acme-artifacts/*"
  }]
}
```

`avc list` also needs `s3:ListBucket` on the bucket. A job that runs `avc push`
needs `s3:PutObject` and `s3:ListBucket` — `push` skips objects the store already
has by asking, and S3 answers a `HEAD` on a missing object with `403` rather than
`404` when `ListBucket` is absent.

Read access to the Git repository and read access to the bucket are separate
grants, and Git is the one that can be scoped to a path. The object store cannot
distinguish projects, because object keys contain no paths.

---

## Exit codes

`avc` never exits `0` on a failed transfer, so `set -e` is enough.

| Code | Meaning | What a pipeline should do |
| --- | --- | --- |
| `0` | Success | Continue |
| `1` | User, data, or state error — a path that names nothing, a pointer that does not match, a refusal to overwrite | Fail the build; retrying will not help |
| `2` | Invalid CLI usage | Fix the command |
| `3` | Provider or operational failure — unreachable endpoint, missing credentials, a ref that does not exist, `git` not installed | Safe to retry |

```bash
avc fetch models/bert -o . || case $? in
  3) echo "::warning::registry or store unreachable, retrying"
     sleep 10; avc fetch models/bert -o . ;;
  *) exit 1 ;;
esac
```

Errors go to stderr as `avc: <message>`.

## Color in logs

Color is on when stdout is a terminal and off otherwise, so a redirected log gets
plain text. Runners that render ANSI in their log viewer can ask for it:

```bash
export CLICOLOR_FORCE=1   # or: avc fetch --color always
```

`NO_COLOR=1`, `--color never`, and `AVC_COLOR=never` all turn it off. Color is
never load-bearing — every line reads the same without it.

## Progress in logs

`fetch`, `push`, and `pull` report how far along a transfer is. In a pipeline
that report is an ordinary line on stdout every ten seconds — never the
in-place bar a workstation gets, because a log is a file read after the fact and
a redrawn line is thousands of `\r`-separated fragments in it:

```text
fetching      62%  5/12 objects  480.0 MiB/1.2 GiB (12.3 MiB/s, eta 0:58)
downloaded   models/bert/weights.bin (878.9 MiB)
```

The present participle is what distinguishes a progress line from the
`downloaded` and `uploaded` lines that record what actually happened, so
`grep -v '^fetching'` leaves the result intact. Nothing appears at all until a
transfer has been running for ten seconds, so a job that moves a few megabytes
prints exactly what it printed before this existed.

A pipeline is recognized by `CI`, `CONTINUOUS_INTEGRATION`, `GITHUB_ACTIONS`,
`GITLAB_CI`, `JENKINS_URL`, `TEAMCITY_VERSION`, or `TF_BUILD` being set to
anything but `0`, `false`, or `no` — and that test wins over the terminal check,
since some runners allocate a pseudo-terminal. A pipe, a redirect, and
`TERM=dumb` are treated the same way. If your runner is not on that list and
gives `avc` a terminal, `--progress never` or `AVC_PROGRESS=never` turns
progress off; `--porcelain` already implies it, since a progress line written
into a machine-readable stream is corruption rather than decoration.

Progress is never the only place a fact appears: the summary each command prints
when it finishes says the same thing, so a job that suppresses it loses nothing.

---

## Recipes

### GitHub Actions

A project that consumes artifacts from a separate registry repository:

```yaml
name: Train
on: [push]

jobs:
  train:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      id-token: write        # for OIDC
    env:
      AVC_REPO: https://github.com/acme/artifacts
      AVC_REF: v2.1.0        # pin it; HEAD is a moving target
    steps:
      - uses: actions/checkout@v4     # this project, not the registry

      - uses: aws-actions/configure-aws-credentials@v4
        with:
          role-to-assume: arn:aws:iam::123456789012:role/avc-read
          aws-region: eu-west-1

      - name: Install avc
        run: cargo install --git https://github.com/jvdavim/avc avc-cli

      - name: Fetch just this project's model
        run: avc fetch models/bert --output .

      - name: Verify before building
        run: avc verify models/bert --output .

      - run: ./train.sh
```

`configure-aws-credentials` exports `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
and `AWS_SESSION_TOKEN`, which is all AVC needs. For a private registry, give
`git` a token:

```yaml
    env:
      AVC_REPO: https://x-access-token:${{ secrets.REGISTRY_TOKEN }}@github.com/acme/artifacts
```

If the registry *is* this repository, drop `--repo` entirely and let `fetch` read
the pointers `actions/checkout` already placed:

```yaml
      - uses: actions/checkout@v4
      - run: avc fetch models/bert
```

### GitLab CI

```yaml
variables:
  AVC_REPO: https://gitlab.example.com/acme/artifacts.git
  AVC_REF: main
  AVC_CACHE_DIR: .avc-cache

cache:
  key:
    files: ["**/*.avc"]
  paths: [.avc-cache]

train:
  stage: build
  script:
    - avc fetch models/bert --output .
    - avc verify models/bert --output .
    - ./train.sh
```

`AVC_CACHE_DIR` and GitLab's content-keyed cache pair well: the key changes only
when a pointer does, which is exactly when the objects change.

### Docker build

Fetch inside its own layer, so the artifacts are re-downloaded only when the
reference actually moves.

```dockerfile
# syntax=docker/dockerfile:1
FROM rust:1.75 AS avc
RUN cargo install --git https://github.com/jvdavim/avc avc-cli

FROM debian:bookworm-slim AS artifacts
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=avc /usr/local/cargo/bin/avc /usr/local/bin/avc
WORKDIR /artifacts
ARG AVC_REF=v2.1.0
RUN --mount=type=secret,id=aws \
    . /run/secrets/aws && \
    avc fetch --repo https://github.com/acme/artifacts --ref "$AVC_REF" \
              models/bert --output .

FROM python:3.12-slim
COPY --from=artifacts /artifacts/models /app/models
```

Pinning `--ref` to a tag is what makes the layer cacheable and the image
reproducible. Use a build secret mount rather than `ARG` for credentials: build
arguments are recorded in the image history.

### A deploy job with nothing checked out

The minimum a deploy needs is the registry URL, a ref, and a path:

```bash
avc fetch --repo https://github.com/acme/artifacts --ref v2.1.0 \
          models/bert --output /srv/app
avc verify --repo https://github.com/acme/artifacts --ref v2.1.0 \
           models/bert --output /srv/app
```

No clone, no `avc init`, no cache, no bucket name anywhere.

### Kubernetes init container

```yaml
initContainers:
  - name: fetch-artifacts
    image: ghcr.io/acme/avc:0.1.0
    args: ["fetch", "models/bert", "--output", "/artifacts"]
    env:
      - name: AVC_REPO
        value: https://github.com/acme/artifacts
      - name: AVC_REF
        value: v2.1.0
      - name: AWS_ACCESS_KEY_ID
        valueFrom: { secretKeyRef: { name: avc-s3, key: access-key-id } }
      - name: AWS_SECRET_ACCESS_KEY
        valueFrom: { secretKeyRef: { name: avc-s3, key: secret-access-key } }
    volumeMounts:
      - { name: artifacts, mountPath: /artifacts }
```

The container exits `0` only once every artifact is on disk and verified, so the
app container starts with the exact bytes that tag names. Nothing has to be
mounted in: the pointers come from Git.

### Publishing to the registry

Producing artifacts still uses the ordinary repository workflow, because writing
a pointer is a change to the repository:

```bash
git clone https://github.com/acme/artifacts && cd artifacts
avc commit models/bert/weights.bin
avc push
git commit -am "Update BERT weights" && git push
git tag v2.2.0 && git push --tags
```

Give that job `s3:PutObject` and `s3:ListBucket`; give every consuming job
`s3:GetObject` only. Tagging is what lets consumers pin.

---

## Troubleshooting

**`no branch, tag, or commit named ...`** (exit `3`) — the revision in `--ref`
does not exist on the server. Check the spelling; remember that omitting `--ref`
means the default branch, and that a name is looked up as a ref before it is
tried as a commit id.

**`no commit in ... matches ...`** (exit `3`) — the revision looked like a commit
id, but no commit on any branch or tag has it as a prefix, or more than one does.
Name more characters, or name the branch or tag instead.

**`could not run git`** (exit `3`) — reading pointers from `--repo` needs the
`git` command on `PATH`. Slim container images often do not have it; install it,
or check out the registry yourself and drop `--repo`.

**`no artifact at models/absent`** (exit `1`) — that path matches nothing at that
reference. `avc list --repo … models` shows what is actually there.

**`… configures no object store`** (exit `1`) — the repository has no
`.avc/config.toml` with a remote in it. Run `avc remote add` in the repository
and commit, or pass `--remote-url` for this run.

**`remote object not found: <hash>`** (exit `1`) — the pointer names an object
that is not in the store. Almost always a commit whose `avc push` never ran.
Check with `avc list --repo … <path>`.

**`refusing to replace <path>: it differs from its pointer; use --force`** — the
workspace is not clean. Add `--force`, or clean the workspace between jobs.

**`invalid peer certificate: UnknownIssuer`** (exit `3`) — the runner is behind
a proxy that inspects TLS and re-signs it with a private CA. Mount your
organization's PEM bundle into the job and set `AVC_CA_BUNDLE` to it, or set
`AVC_SYSTEM_CERTS=1` if the image already carries the CA in its trust store. A
self-hosted runner on a corporate network is the usual place this appears; see
[Configuration](configuration.md#tls-and-corporate-proxies). There is no option
to skip verification.

**`cannot read the CA bundle at …`** (exit `3`) — `AVC_CA_BUNDLE` points at a
path that is not in the container. Mount it, or bake it into the image; a path
that exists on the runner host is not a path that exists inside the job.

**`no credentials found for profile 'default'`** (exit `3`) — the environment
variables are not reaching the process. Secrets are commonly scoped to a specific
job or environment; confirm they are exported in the step that runs `avc`.

**`provider adapter not implemented: gcs`** (exit `3`) — `gs://` and `az://`
parse and configure but do not transfer yet. See the [roadmap](roadmap.md).

**A large fetch fails partway through** — there is no resumable download yet, so
a dropped connection restarts that object. Objects already written are not
re-fetched, so re-running the same `avc fetch` resumes at object granularity.

## See also

- [CLI Reference](cli.md) — every command and flag
- [Configuration](configuration.md) — remote URLs and the credential chain
- [Concepts](concepts.md) — what a pointer, an object, and a manifest are
