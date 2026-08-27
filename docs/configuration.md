# Configuration

AVC keeps configuration in two files under `.avc/`.

| File | Tracked by Git? | Contains |
| --- | --- | --- |
| `.avc/config.toml` | **Yes** — commit it | Providers, buckets, prefixes, endpoints, remote names |
| `.avc/config.local.toml` | No — gitignored | Machine-local overrides |

The split exists so a team shares *where artifacts live* without sharing *how to
authenticate*.

## `.avc/config.toml`

Written by `avc init` and updated by `avc remote add`. It is TOML and can be
edited by hand.

```toml
default_remote = "origin"

[[remotes]]
name = "origin"
provider = "s3"
bucket_or_container = "my-bucket"
prefix = "artifacts"

[[remotes]]
name = "minio"
provider = "s3"
bucket_or_container = "my-bucket"
prefix = "artifacts"
endpoint_url = "https://storage.example.com"

[[remotes]]
name = "local"
provider = "file"
bucket_or_container = "/tmp/avc-remote"
prefix = ""
```

### Fields

| Field | Type | Meaning |
| --- | --- | --- |
| `default_remote` | string, optional | Remote used when `--remote` is omitted |
| `remotes[].name` | string | Identifier passed to `--remote` |
| `remotes[].provider` | `file` \| `s3` \| `gcs` \| `azure` | Transport family |
| `remotes[].bucket_or_container` | string | Bucket, container, or — for `file` — an absolute directory path |
| `remotes[].prefix` | string, optional | Key prefix inside the bucket |
| `remotes[].endpoint_url` | string, optional | Custom endpoint for S3-compatible services |

An empty `config.toml` (as written by `init`, which seeds it with only a comment)
is valid and parses as "no remotes configured."

## Remote URLs

`avc remote add` takes a URL and decomposes it. **The provider is determined by
the URL scheme and nothing else.** AVC never guesses a provider from a hostname,
because a wrong guess means sending credentials to an unintended endpoint.

### `file://` — local directory

```bash
avc remote add local file:///srv/artifacts
```

| Field | Value |
| --- | --- |
| `provider` | `file` |
| `bucket_or_container` | `/srv/artifacts` |
| `prefix` | *(empty)* |

The only transport implemented in `0.1.0`. Useful for offline development,
testing, and a shared network mount. The path must be absolute — `file://` URLs
have no notion of a relative path.

### `s3://` — Amazon S3

```bash
avc remote add origin s3://my-bucket/artifacts/v1
```

| Field | Value |
| --- | --- |
| `provider` | `s3` |
| `bucket_or_container` | `my-bucket` |
| `prefix` | `artifacts/v1` |

The prefix may be omitted: `s3://my-bucket` is valid and stores objects at the
bucket root.

### `s3+https://` — S3-compatible services

For MinIO, Cloudflare R2, Ceph, Backblaze B2, and similar. The host becomes the
endpoint; the **first path segment is the bucket**, and the rest is the prefix.

```bash
avc remote add minio s3+https://storage.example.com/my-bucket/artifacts
```

| Field | Value |
| --- | --- |
| `provider` | `s3` |
| `bucket_or_container` | `my-bucket` |
| `prefix` | `artifacts` |
| `endpoint_url` | `https://storage.example.com` |

A bucket segment is required; `s3+https://storage.example.com/` is rejected.

### `gs://` — Google Cloud Storage

```bash
avc remote add origin gs://my-bucket/artifacts
```

Host is the bucket, path is the prefix.

### `az://` — Azure Blob Storage

```bash
avc remote add origin az://my-container/artifacts
```

Host is the container, path is the prefix.

### Rejected URLs

| URL | Why |
| --- | --- |
| `https://my-bucket/path` | No provider is implied by `https`. Use `s3+https://` if you mean S3-compatible. |
| `s3://` | No bucket. |
| `my-bucket/path` | Not a URL. |
| `ftp://host/path` | Unsupported scheme. |

## Object key layout on a remote

A remote mirrors the cache layout. The full key is:

```text
<prefix>/objects/sha256/<first-two-hash-characters>/<full-hash>
```

For `s3://my-bucket/artifacts` and hash `1dfc4d10…`:

```text
s3://my-bucket/artifacts/objects/sha256/1d/1dfc4d10…
```

Keys contain hashes only — never repository paths. A bucket shared across teams
does not reveal one team's directory structure to another. It does reveal object
sizes and the fact that two repositories reference identical content.

## Credentials

**Never put credentials in `.avc/config.toml`.** It is committed to Git.

`0.1.0` does not perform any cloud authentication, because no cloud adapter is
implemented. When the adapters land, the intended precedence is:

1. Provider-standard credential chains — `AWS_ACCESS_KEY_ID` and friends,
   `~/.aws/credentials`, IAM instance roles, `GOOGLE_APPLICATION_CREDENTIALS`,
   Azure managed identity.
2. `.avc/config.local.toml`, for machine-specific overrides.

Provider-standard chains are preferred so AVC does not become another place
secrets can leak from. See [Roadmap](roadmap.md).

## `.avc/config.local.toml`

Gitignored by `avc init`. Reserved for machine-local overrides such as an
alternate endpoint for a developer's local MinIO, or credentials that cannot come
from a provider chain.

It is **not read by `0.1.0`** — the file is created as ignored so that adding it
later is not a breaking change.

## `.gitignore` management

AVC edits `.gitignore` in two places:

- `avc init` appends `.avc/cache/` and `.avc/config.local.toml`. It does not add
  `.avc/state/`, which is currently empty and therefore invisible to Git anyway.
- `avc add` and `avc commit` append the artifact's repository-relative path.

Entries are appended only when an exact matching line is absent, so running the
commands repeatedly does not duplicate lines. Existing content is preserved and a
trailing newline is added if missing.

`avc remove` does **not** remove the `.gitignore` line; delete it by hand if you
want Git to track the file directly again.

## What to commit

```bash
git add .avc/config.toml .gitignore model.bin.avc
git commit -m "Track model artifact"
```

| Path | Commit? |
| --- | --- |
| `.avc/config.toml` | Yes |
| `*.avc` pointer files | Yes |
| `.gitignore` | Yes |
| `.avc/cache/` | No — gitignored by `init` |
| `.avc/state/` | No — empty, so Git ignores it implicitly |
| `.avc/config.local.toml` | No — gitignored |
| The artifacts themselves | No — gitignored by `avc add` |
