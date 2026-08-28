# AVC Documentation

AVC (Artifact Version Control) tracks large files alongside Git without requiring
Git LFS. Git stores small YAML pointer files; AVC stores the artifact bytes in a
local content-addressed cache and synchronizes them with an object store.

> **Status:** `0.1.0` prototype. The on-disk formats are an *Iteration 0 contract*
> and remain provisional. See [`../SPEC.md`](../SPEC.md) for the normative rules
> and [Roadmap](roadmap.md) for what is not built yet.

## Start here

| If you want to… | Read |
| --- | --- |
| Install AVC and track your first artifact | [Getting Started](getting-started.md) |
| Understand pointers, objects, and the cache | [Concepts](concepts.md) |
| Look up a command, flag, or exit code | [CLI Reference](cli.md) |
| Use AVC in a build pipeline | [CI/CD](ci-cd.md) |
| Configure remotes and credentials | [Configuration](configuration.md) |
| Learn how the crates fit together | [Architecture](architecture.md) |
| Contribute code, docs, or bug reports | [Contributing](contributing.md) |
| Set up a dev environment and run the checks | [Development](development.md) |
| See what is planned and what is missing | [Roadmap](roadmap.md) |

## What AVC is

Machine-learning repositories, game projects, and data pipelines routinely carry
files that Git handles poorly: model weights, datasets, textures, archives.
Git stores every version of every file in the repository history, so a 4 GB
checkpoint committed ten times becomes a 40 GB clone for everyone forever.

AVC keeps those bytes out of Git. Running `avc add model.bin`:

1. Streams `model.bin` through SHA-256 to derive a content address.
2. Copies the bytes into `.avc/cache` under that address.
3. Writes a small pointer file, `model.bin.avc`, that you commit to Git.
4. Adds `model.bin` itself to `.gitignore`.

`avc add data/` does the same for a whole directory: every file beneath it is
hashed and cached, and a manifest naming them becomes one more object, so the
directory is a single artifact behind a single `data.avc` pointer — see
[Concepts](concepts.md#directories).

In a pipeline, `avc fetch` skips all of that: it downloads artifacts straight
from the remote to the paths their pointers name, with no clone, no `avc init`,
and no cache — see [CI/CD](ci-cd.md).

`avc push` uploads the cached bytes to a remote — a local directory, Amazon S3,
or any S3-compatible service such as MinIO, Cloudflare R2, Ceph, or Backblaze B2.
Cloning the repository gives you the pointer. `avc pull` fetches the bytes back,
verifying each object against its pointer as it downloads.
Git history stays small, and the pointer gives you an exact, verifiable identity
for the artifact at every commit.

## What AVC is not

- **Not a Git replacement.** AVC has no history model of its own. Versioning
  comes entirely from Git commits of the pointer files.
- **Not Git LFS.** AVC needs no server-side Git hooks, no smudge/clean filters,
  and no `git lfs install` on every clone. The trade-off is that artifact
  materialization is an explicit `avc pull`, not automatic on checkout.
- **Not yet a universal cloud tool.** `file://`, `s3://`, `s3+https://`, and
  `s3+http://` remotes move bytes — Amazon S3 and anything that speaks the S3
  API. `gs://` and `az://` URLs still parse and store correctly, but a transfer
  returns an explicit unsupported-provider error.

## Design commitments

These hold across the prototype and are enforced by [`SPEC.md`](../SPEC.md):

- **Content addressing.** SHA-256 over exact file bytes, streamed in 64 KiB
  chunks so memory stays bounded regardless of artifact size.
- **No path leakage.** Object keys contain hashes only. A remote bucket never
  learns your repository's directory structure.
- **Atomic writes.** Every cache and worktree write goes to a temporary file,
  is `fsync`ed, then renamed into place.
- **No silent data loss.** Modified working-tree files are never overwritten
  without `--force`. No command deletes remote data.
- **Explicit providers.** A provider is chosen by URL scheme, never inferred
  from a hostname.
- **Credentials stay out of Git.** Tracked config holds only bucket, prefix, and
  endpoint. Keys come from the environment, an ignored `.avc/config.local.toml`,
  or `~/.aws/credentials`.

## Project layout

```text
avc/
├── Cargo.toml              # workspace manifest
├── SPEC.md                 # normative format and safety contract
├── docs/                   # this documentation
└── crates/
    ├── avc-core/           # domain types: pointers, hashing, paths, remotes
    │                       #   remote/: file and S3 transport, SigV4 signing
    └── avc-cli/            # the `avc` binary and MVP workflows
                            #   ci.rs: fetch and verify, built for pipelines
                            #   ui.rs: ASCII tables and color detection
```

## License

AVC is licensed under the MIT License. See [`../LICENSE`](../LICENSE).
