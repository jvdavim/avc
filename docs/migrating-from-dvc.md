# Migrating from DVC

`avc migrate dvc` turns a DVC project into an AVC one: the whole Git history,
every branch and tag, and every object any of those commits references.

```bash
avc migrate dvc \
  https://github.com/acme/ml-project \
  s3://acme-dvc-storage \
  --into ./ml-project-avc \
  --to s3://acme-dvc-storage/avc
```

The two positional arguments are the DVC side — the repository and the remote
holding its data. `--into` and `--to` are the AVC side — where the new
repository is written and which object store it will use. Nothing about the DVC
project is modified; both of its halves are read only.

## Read this first if your remote is large

Naming the **DVC remote's own bucket** in `--to` is the difference between a
migration that takes minutes and one that takes days.

AVC normally addresses objects by SHA-256 and DVC addresses them by MD5.
Re-addressing a DVC object would mean reading every byte of it — and a migration
reads not just the current version of each artifact but *every version in the
history*. On a real remote that is a download measured in terabytes, and it is
the reason migrations get postponed forever.

So by default it does not do that. AVC records the algorithm alongside every
digest, so a migrated artifact keeps the MD5 identity DVC already gave it. The
object never has to be read to work out where it goes — and when the destination
is on the same S3 service, it never has to be read at all:

```
transferred  48210 objects copied by the storage service, no bytes over the network
```

That is a server-side `CopyObject`. The bytes move inside the storage service,
your machine sees none of them, and you pay no egress.

Point `--to` at a different service — a different provider, a different account,
a local `file://` directory — and the objects are streamed through this machine
instead. That works and is correct; it is just as slow as the transfer is large.

Same bucket is fine. AVC's keys are `objects/md5/…` and `objects/sha256/…`,
which no DVC layout uses, so the two sets of objects sit side by side without
touching each other. Your DVC remote keeps working throughout, which means you
can migrate before you are ready to cut over.

### The `--rehash` alternative

```bash
avc migrate dvc … --rehash
```

Reads every object and re-addresses it with SHA-256, leaving a repository with
no MD5 in it. This is the stronger end state: MD5 collisions are generatable in
seconds, and in a content-addressed store that means two different files can
claim one address — including in the artifacts `avc verify` gates a pipeline on.

It costs a full read of every version of every artifact. For a store measured in
gigabytes, take it. For one measured in terabytes, migrate without it and decide
later; nothing about the default forecloses re-hashing afterwards.

## What comes across

| DVC | becomes |
| --- | --- |
| `model.bin.dvc` | `model.bin.avc` |
| a `.dvc` file tracking a directory | an `.avc` directory pointer plus a rewritten manifest |
| `outs:` of each `dvc.lock` stage | an `.avc` pointer per output |
| objects on the DVC remote | objects under `objects/md5/…` on the AVC remote |
| `.dvc/`, `.dvcignore`, `dvc.lock` | removed (keep them with `--keep-dvc-files`) |
| `dvc.yaml` | kept — it is your pipeline definition, not DVC's bookkeeping |
| every branch, tag, and merge | the same graph, replayed |
| authors, dates, messages | preserved exactly, zone offsets included |

DVC's directory manifests are rewritten rather than copied: DVC's `.dir` file
records a hash and a path per file and no sizes, while AVC's manifest records
sizes too, because that is what lets `avc list` report a directory's size and a
pull show a real progress total without downloading anything. The rewritten
manifest is a new object, about a hundred bytes per file it names.

### What does not

- **GPG signatures.** A signature signs content that no longer exists. Signed
  commits are replayed unsigned.
- **Annotated tags** become lightweight tags at the rewritten commit.
- **Outs DVC never cached** (`cache: false`), outs tracked by cloud versioning
  rather than by content, and outs hashed with anything but MD5. Each is
  reported by name at the end of the run.
- **Objects missing from the DVC remote** — a remote that has been
  garbage-collected legitimately no longer holds every version its history
  mentions. These are reported, and the `.dvc` file that named them is left in
  place rather than deleted, so the record of what the artifact was survives.

## Where the migrated history lands

**Into a new or empty directory,** the migrated project *is* the repository, so
its branches keep their names:

```
DVC   AVC
dev   dev
main  main
```

**Into a repository that already has commits,** the migrated refs are prefixed
so that nothing already there is touched or shadowed:

```
DVC   AVC
dev   dvc-dev
main  dvc-main
```

Tags are prefixed the same way (`v1.0` becomes `dvc-v1.0`). Your existing
branches, tags, and working tree are left exactly as they were. Change the
prefix with `--branch-prefix`.

The two histories are unrelated, which Git is entirely happy with — the migrated
branch is a separate line of development you can merge, cherry-pick from, or
leave alone.

## Resuming

A migration that stops — a dropped connection, a full disk, a closed laptop —
resumes. Re-run the same command:

```
inventory    already taken (48210 objects)
survey       already taken
moving  ████████████░░░░░░░░  61%  2.1 TB/3.4 TB  118 MB/s  ~3h11m
```

Progress is recorded as each unit of work finishes, in
`.avc/state/migrate/` inside the destination. The remote is not listed again,
transferred objects are not re-sent, and rewritten commits are not rebuilt. The
state directory is removed when the migration completes.

Resuming is keyed to the arguments: a journal recording a different source or
destination is refused rather than continued, because resuming it would mix two
migrations into one repository. `--restart` discards it and starts over.

## The phases

Watching a migration, this is what the lines mean.

1. **inventory** — one listing of the DVC remote, which answers two questions at
   once: which key layout it uses (DVC 3 puts objects under `files/md5/`, DVC 2
   and earlier at the root — detected, or forced with `--dvc-layout`), and how
   large every object is. DVC's directory manifests record no sizes, so without
   this the migration could not report a total before starting.
2. **survey** — every commit is read and every `.dvc` file and `dvc.lock` parsed,
   building the complete set of objects the history needs. Not just the tips: an
   artifact replaced five years ago is still referenced by the commit that
   replaced it.
3. **transfer** — the objects move, by server-side copy where possible.
4. **manifests** — DVC directory manifests are rewritten in AVC's format and
   uploaded, always after the objects they name.
5. **replay** — each commit is rebuilt with its pointers translated. This is
   local and fast; it assembles each tree in a temporary index rather than
   checking anything out, so it touches only the entries that change.
6. **refs** — branches and tags are pointed at the rewritten commits.

## After the migration

```bash
cd ml-project-avc
avc status          # what came across
avc pull            # bring the current artifacts down
git remote add origin git@github.com:acme/ml-project-avc.git
git push --all && git push --tags
```

Nothing about the DVC project has changed, so you can run both until you are
confident. When you are, the DVC objects in the bucket are yours to delete;
AVC's live under `objects/` and are untouched by anything that removes DVC's.

## Credentials

The two ends are configured independently:

```bash
avc migrate dvc … \
  --from-profile dvc-account   --from-region us-east-1 \
  --to-profile   avc-account   --to-region   us-east-1
```

Without those flags both ends use the ambient credentials — environment
variables, then `~/.aws`. For an S3-compatible service, spell the endpoint in
the URL: `s3+https://minio.internal/bucket/prefix`. Machine-local overrides in
the destination's `.avc/config.local.toml` apply to the destination remote; see
[Configuration](configuration.md).

A server-side copy needs one set of credentials able to read the source and
write the destination, which is automatic when both are the same bucket.

## Options

| Flag | Effect |
| --- | --- |
| `--into <DIR>` | Where the AVC repository is written. Required. |
| `--to <URL>` | Object store the migrated repository will use. Required. |
| `--rehash` | Re-address every object with SHA-256, reading all of them. |
| `--keep-dvc-files` | Leave `.dvc` files, `.dvc/`, `.dvcignore` and `dvc.lock` in the history. |
| `--branch-prefix <P>` | Prefix for migrated refs when the destination already has commits. Default `dvc-`. |
| `--remote-name <N>` | Name to record the remote under. Default `origin`. |
| `--dvc-layout <L>` | `auto`, `files-md5`, or `legacy`. Default `auto`. |
| `--from-region`, `--from-profile` | Credentials for the DVC remote. |
| `--to-region`, `--to-profile` | Credentials for the destination remote. |
| `--restart` | Discard recorded progress and migrate from the beginning. |

## Requirements

`git` on `PATH`, and enough disk for the destination repository's Git history —
which is pointer files and source, not artifacts. With `--rehash`, one object at
a time is buffered under `.avc/cache/`, so the peak is the size of the largest
single object.
