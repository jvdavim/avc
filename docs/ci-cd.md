# AVC in CI/CD

How to get artifacts into a build, and how to prove you got the right ones.

Two commands exist for this and nothing else:

| Command | What it does | What it needs |
| --- | --- | --- |
| [`avc fetch`](#avc-fetch) | Downloads artifacts straight from the remote | A remote URL, credentials, and pointer files |
| [`avc verify`](#avc-verify) | Checks artifacts on disk against their pointers | Pointer files and the artifacts |

Neither needs a Git repository, an `avc init`, or a local cache.

## The 30-second version

```yaml
- run: avc fetch --remote-url s3://my-bucket/artifacts --output .
  env:
    AWS_ACCESS_KEY_ID: ${{ secrets.AWS_ACCESS_KEY_ID }}
    AWS_SECRET_ACCESS_KEY: ${{ secrets.AWS_SECRET_ACCESS_KEY }}
```

That scans the checkout for `.avc` pointer files, downloads exactly the objects
they name, verifies each one as it streams, and writes it to the path its
pointer says. Nothing else is written — no cache, no state directory, no
`.gitignore` edits.

---

## Why not `avc pull`?

`avc pull` is built for a workstation. In a pipeline it is the wrong shape:

| | `avc pull` | `avc fetch` |
| --- | --- | --- |
| Git worktree | required | not used |
| `avc init` / `.avc/config.toml` | required | not used |
| Remote | from tracked config | from a URL or `$AVC_REMOTE_URL` |
| Local cache | always populated | none by default, `--cache` optional |
| Disk used | 2x the artifact size | 1x |
| S3 permissions | `GetObject`, and `ListBucket` for `list` | `GetObject` only |
| Selecting artifacts | repository-relative paths | pointer files, a directory, or stdin |

The disk difference is not academic. `pull` writes every object into
`.avc/cache` and then copies it into the worktree, so a job pulling 40 GB of
checkpoints needs 80 GB of runner disk — and then throws the cache away when the
job ends. `fetch` streams from the remote to the destination path, hashing as it
writes, so a 40 GB artifact costs 40 GB and a few megabytes of RSS.

Use `pull` when the job really is a checkout of a developer's repository and you
want the cache. Use `fetch` everywhere else.

---

## `avc fetch`

```text
avc fetch [<pointer>...] [--remote-url <url> | --remote <name>]
          [--output <dir>] [--cache <dir>]
          [--force] [--dry-run] [--porcelain]
```

### Choosing the remote

One of these, in order of precedence:

| Source | Example | Repository needed |
| --- | --- | --- |
| `--remote-url` | `--remote-url s3://my-bucket/artifacts` | no |
| `$AVC_REMOTE_URL` | `AVC_REMOTE_URL=s3://my-bucket/artifacts` | no |
| `--remote <name>` | `--remote origin` | yes — reads `.avc/config.toml` |
| the default remote | *(nothing)* | yes — reads `.avc/config.toml` |

`--remote-url` takes any URL `avc remote add` takes: `s3://`, `s3+https://`,
`s3+http://`, and `file://`. Setting `AVC_REMOTE_URL` once at the top of a
pipeline keeps every job's command line to `avc fetch`.

If neither a URL nor a repository is available, the error says so:

```text
avc: not inside a Git worktree; outside a repository, name the remote with
--remote-url <url> or set AVC_REMOTE_URL
```

### Choosing the artifacts

| Argument | Selects |
| --- | --- |
| *(none)* | every `.avc` pointer beneath the current directory |
| `model.bin.avc` | that one artifact |
| `models/` | every `.avc` pointer beneath `models/` |
| `-` | newline-separated pointer paths read from stdin |

`.git`, `.avc`, and `target` are skipped when scanning, and symlinks are never
followed. Results are sorted by artifact path, so two runs of the same job
produce the same log.

Stdin is there so a pipeline can select with the tools it already has:

```bash
# only the artifacts this commit changed
git diff --name-only HEAD~1 -- '*.avc' | avc fetch -
```

Naming a pointer that does not exist is an error, not an empty selection — a
typo in a pipeline should fail the job rather than silently fetch nothing.

### Where files land

`--output` (`-o`) is the root the pointers' paths are resolved against; it
defaults to the current directory. A pointer for `models/final.safetensors`
fetched with `-o /srv/app` lands at `/srv/app/models/final.safetensors`. Parent
directories are created as needed.

A directory artifact materializes as its files and nothing else — the manifest
that names them stays in AVC's bookkeeping and is never written into the output
tree.

### Re-running a job

`fetch` is idempotent and cheap to repeat. Before transferring anything it
checks what is already on disk:

```text
up-to-date   model.bin (4.0 GiB)
```

A file whose contents already hash to what the pointer claims is left alone and
costs one local read instead of a download. That makes `fetch` safe to put in a
step that reruns, and makes a retried job skip everything the first attempt
finished.

A file that exists but **differs** is a refusal, not an overwrite:

```text
avc: refusing to replace data/a.bin: it differs from its pointer; use --force
```

A fresh workspace never hits this. A reused runner workspace might, and
`--force` is the answer there — it is the same rule, and the same escape hatch,
that `avc checkout` uses.

### Caching between jobs

`--cache <dir>` (or `$AVC_CACHE_DIR`) makes `fetch` read from and write to a
content-addressed cache directory, which a runner can persist between jobs:

```yaml
- uses: actions/cache@v4
  with:
    path: .avc-cache
    key: avc-${{ hashFiles('**/*.avc') }}
- run: avc fetch --cache .avc-cache --output .
```

Cache entries are verified by re-hashing before they are used, and an entry that
fails is deleted and re-downloaded. The key above is content-addressed by
construction: when no pointer changed, nothing was fetched.

This costs disk — the cache holds a second copy of every object — so it is worth
it when the artifacts are large *and* the runner's cache is faster than your
object store. Without `--cache`, nothing is duplicated.

### Output

```text
fetching 3 artifacts from https://s3.eu-west-1.amazonaws.com/my-bucket
  into       .

downloaded   models/final.safetensors (4.0 GiB)
from-cache   data/ (1204 files, 812.5 MiB)
up-to-date   config.bin (2.1 KiB)

fetched 1 object (4.0 GiB) for 3 artifacts from https://s3.eu-west-1.amazonaws.com/my-bucket
```

| Word | Meaning |
| --- | --- |
| `downloaded` | Bytes came over the network |
| `from-cache` | Served from `--cache`, or from an identical object already fetched this run |
| `up-to-date` | Already on disk and already correct; nothing was written |
| `would-fetch` | `--dry-run`: this is what a real run would transfer |

The object and byte counts are artifact content only. Reading a directory's
manifest is not counted: it is a few bytes of metadata `fetch` must read to know
what else to ask for, and counting it would make a directory that is entirely
up to date report a download on every run.

Identical files are transferred once even with no cache, whether they are two
paths inside one directory artifact or two separate artifacts.

### Machine-readable output

`--porcelain` prints one tab-separated line per artifact and nothing else:

```text
downloaded	1	4294967296	models/final.safetensors
up-to-date	0	0	config.bin
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
avc fetch --dry-run --porcelain | awk -F'\t' '{ bytes += $3 } END { print bytes }'
```

It still reads directory manifests, because that is the only way to know what a
directory contains. It never reads or writes artifact bytes.

---

## `avc verify`

```text
avc verify [<pointer>...] [--output <dir>] [--porcelain]
```

Re-hashes what is on disk and compares it with what the pointers claim, using
nothing but the two — no remote, no credentials, no cache, no repository. It
exits `1` if anything is missing or differs, which makes it a gate.

```text
STATUS         SIZE  ARTIFACT
ok        195.3 KiB  models/final.safetensors
modified       20 B  data/
missing           -  config.bin

3 artifacts checked: 1 ok, 2 not matching
```

| Status | Meaning |
| --- | --- |
| `ok` | Present, and its bytes hash to what the pointer says |
| `modified` | Present, and its bytes do not |
| `missing` | Not there at all |

For a directory, `modified` covers a file edited, added, removed, or renamed
anywhere beneath it: the directory's identity is the hash of the manifest of its
contents, so any of those changes it.

`--porcelain` prints `<status>\t<bytes on disk>\t<path>`.

Useful places for it:

- **After a fetch into a workspace you do not control**, before you trust it.
- **After a build**, to prove nothing overwrote an input.
- **At the start of a job that inherited a workspace** from an earlier stage, to
  fail fast rather than build against half a dataset.

`avc verify` accepts the same pointer selection as `avc fetch`.

> Finding no pointers is not a failure: it prints `no AVC pointers found` and
> exits `0`. If a gate must not pass on an empty selection, name the pointers
> explicitly — `avc verify models/final.safetensors.avc` — rather than relying
> on the directory scan.

---

## Credentials

AVC reads provider-standard environment variables first, which is where every CI
system puts secrets. Nothing needs to be written to disk.

| Variable | Purpose |
| --- | --- |
| `AWS_ACCESS_KEY_ID` | Required for S3 remotes |
| `AWS_SECRET_ACCESS_KEY` | Required for S3 remotes |
| `AWS_SESSION_TOKEN` | Temporary credentials — set this when using OIDC or `assume-role` |
| `AWS_REGION` / `AWS_DEFAULT_REGION` | Signing region; defaults to `us-east-1` |
| `AWS_ENDPOINT_URL_S3` / `AWS_ENDPOINT_URL` | Overrides the endpoint, for MinIO, R2, Ceph |

> **Not supported yet:** AVC does not call instance-metadata endpoints, so IAM
> instance roles, ECS task roles, and SSO do not work on their own. Federated
> credentials still work as long as something exchanges them for the three
> environment variables above first — which is exactly what
> `aws-actions/configure-aws-credentials` and `assume-role` wrappers do.

### Least privilege

`avc fetch` issues `GetObject` requests and nothing else. A fetch-only job
should have a policy that says so:

```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Action": "s3:GetObject",
    "Resource": "arn:aws:s3:::my-bucket/artifacts/*"
  }]
}
```

A job that also runs `avc push` needs `s3:PutObject` and `s3:ListBucket` on the
bucket — `push` skips objects the remote already has by asking, and S3 answers a
`HEAD` on a missing object with `403` rather than `404` when `ListBucket` is
absent.

---

## Exit codes

`avc` never exits `0` on a failed transfer, so `set -e` is enough.

| Code | Meaning | What a pipeline should do |
| --- | --- | --- |
| `0` | Success | Continue |
| `1` | User, data, or state error — a pointer that does not match, a refusal to overwrite | Fail the build; retrying will not help |
| `2` | Invalid CLI usage | Fix the command |
| `3` | Provider or operational failure — unreachable endpoint, bad signature, missing credentials | Safe to retry |

```bash
avc fetch --output . || case $? in
  3) echo "::warning::object store unreachable, retrying"; sleep 10; avc fetch --output . ;;
  *) exit 1 ;;
esac
```

Errors go to stderr as `avc: <message>`.

## Color in logs

Color is on when stdout is a terminal and off otherwise, so a redirected log
gets plain text. Runners that render ANSI in their log viewer can ask for it:

```bash
export CLICOLOR_FORCE=1   # or: avc fetch --color always
```

`NO_COLOR=1`, `--color never`, and `AVC_COLOR=never` all turn it off. Color is
never load-bearing — every line reads the same without it.

---

## Recipes

### GitHub Actions

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
      AVC_REMOTE_URL: s3://my-bucket/artifacts
    steps:
      - uses: actions/checkout@v4

      - uses: aws-actions/configure-aws-credentials@v4
        with:
          role-to-assume: arn:aws:iam::123456789012:role/avc-read
          aws-region: eu-west-1

      - name: Install avc
        run: cargo install --git https://github.com/jvdavim/avc avc-cli

      - name: Fetch artifacts
        run: avc fetch --output .

      - name: Verify before building
        run: avc verify --output .

      - run: ./train.sh
```

`configure-aws-credentials` exports `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, and `AWS_SESSION_TOKEN`, which is all AVC needs.

To fetch only what a pull request touched:

```yaml
      - run: |
          git diff --name-only origin/${{ github.base_ref }}...HEAD -- '*.avc' \
            | avc fetch -
```

### GitLab CI

```yaml
variables:
  AVC_REMOTE_URL: s3://my-bucket/artifacts
  AVC_CACHE_DIR: .avc-cache

cache:
  key:
    files: ["**/*.avc"]
  paths: [.avc-cache]

fetch:
  stage: build
  script:
    - avc fetch --output .
    - avc verify --output .
```

`AVC_CACHE_DIR` and GitLab's content-keyed cache pair well: the key changes only
when a pointer does, which is exactly when the objects change.

### Docker build

Pointers are small and cache-friendly, so copy them first and fetch in their own
layer. The artifacts are re-fetched only when a pointer actually changes.

```dockerfile
# syntax=docker/dockerfile:1
FROM rust:1.75 AS avc
RUN cargo install --git https://github.com/jvdavim/avc avc-cli

FROM debian:bookworm-slim AS artifacts
COPY --from=avc /usr/local/cargo/bin/avc /usr/local/bin/avc
WORKDIR /artifacts
COPY models/*.avc models/
RUN --mount=type=secret,id=aws \
    . /run/secrets/aws && \
    avc fetch --remote-url s3://my-bucket/artifacts --output .

FROM python:3.12-slim
COPY --from=artifacts /artifacts/models /app/models
```

Use a build secret mount rather than `ARG` for credentials: build arguments are
recorded in the image history.

### A deploy job with no repository at all

The minimum AVC needs is the pointer file. Commit it, ship it in the artifact
bundle, or fetch it from your Git host — then:

```bash
curl -sSfLO https://git.example.com/api/v4/projects/1/repository/files/model.bin.avc/raw
avc fetch model.bin.avc \
  --remote-url s3://my-bucket/artifacts \
  --output /srv/app
avc verify model.bin.avc --output /srv/app
```

No clone, no `avc init`, no cache — just a pointer, a URL, and credentials.

### Kubernetes init container

```yaml
initContainers:
  - name: fetch-artifacts
    image: ghcr.io/example/avc:0.1.0
    args: ["fetch", "--output", "/artifacts"]
    env:
      - name: AVC_REMOTE_URL
        value: s3://my-bucket/artifacts
      - name: AWS_ACCESS_KEY_ID
        valueFrom: { secretKeyRef: { name: avc-s3, key: access-key-id } }
      - name: AWS_SECRET_ACCESS_KEY
        valueFrom: { secretKeyRef: { name: avc-s3, key: secret-access-key } }
    volumeMounts:
      - { name: artifacts, mountPath: /artifacts }
      - { name: pointers,  mountPath: /workspace }
    workingDir: /workspace
```

Mount the pointer files at `workingDir` (a ConfigMap works — they are a few
hundred bytes) and the shared volume at `--output`. The container exits `0` only
once every artifact is on disk and verified, so the app container starts with
the exact bytes the commit named.

### Publishing from CI

Producing artifacts still uses the ordinary repository workflow, because writing
a pointer is a change to the repository:

```bash
avc commit models/final.safetensors
avc push
git add models/final.safetensors.avc
git commit -m "Update model"
git push
```

Give that job `s3:PutObject` and `s3:ListBucket`; give every consuming job
`s3:GetObject` only.

---

## Troubleshooting

**`remote object not found: <hash>`** — the pointer names an object that is not
on the remote. Almost always a commit whose `avc push` never ran, or a push to a
different bucket or prefix than the one the job reads. Check with
`avc list --remote <name>` from a checkout.

**`refusing to replace <path>: it differs from its pointer; use --force`** — the
workspace is not clean. Add `--force`, or clean the workspace between jobs.

**`no credentials found for profile 'default'`** (exit `3`) — the environment
variables are not reaching the process. Secrets are commonly scoped to a
specific job or environment; confirm they are exported in the step that runs
`avc`.

**`provider adapter not implemented: gcs`** (exit `3`) — `gs://` and `az://`
parse and configure but do not transfer yet. See the [roadmap](roadmap.md).

**A large fetch fails partway through** — there is no resumable download yet, so
a dropped connection restarts that object. Objects already written are not
re-fetched, so re-running the same `avc fetch` resumes at object granularity.

**`no AVC pointers found`** — the job is not in the directory holding the
pointers, or the checkout did not include them. `fetch` scans the current
directory, not `--output`.

## See also

- [CLI Reference](cli.md) — every command and flag
- [Configuration](configuration.md) — remote URLs and the credential chain
- [Concepts](concepts.md) — what a pointer, an object, and a manifest are
