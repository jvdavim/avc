# Configuration

AVC keeps configuration in two files under `.avc/`.

| File | Tracked by Git? | Contains |
| --- | --- | --- |
| `.avc/config.toml` | **Yes** — commit it | Providers, buckets, prefixes, endpoints, regions, profile names, remote names |
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
region = "sa-east-1"
profile = "artifacts"

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
| `remotes[].region` | string, optional | SigV4 signing region, and the region the bucket is in |
| `remotes[].profile` | string, optional | Section of `~/.aws/config` and `~/.aws/credentials` to authenticate with |

`region` and `profile` hold *names*, never secrets, which is why they belong in
the committed file: a clone reaches the right bucket in the right region through
the right profile with no local setup. Both are set by `avc remote add --region`
and `--profile`, and both are overridden by the environment and by
`config.local.toml` — see [Credentials](#credentials).

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

Useful for offline development, testing, and a shared network mount: it needs no
credentials and no network. The path must be absolute — `file://` URLs have no
notion of a relative path.

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

### `s3+https://` and `s3+http://` — S3-compatible services

For MinIO, Cloudflare R2, Ceph, Backblaze B2, and similar. The host becomes the
endpoint; the **first path segment is the bucket**, and the rest is the prefix.
A port, if given, is preserved.

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

`s3+http://` is the same thing without TLS, for a MinIO or Ceph instance on a
trusted network:

```bash
avc remote add minio s3+http://localhost:9000/my-bucket/artifacts
```

The scheme is spelled out rather than inferred, so nobody sends credentials in
the clear without having typed the word `http`.

### Addressing style

Amazon S3 is addressed virtual-hosted-style
(`https://my-bucket.s3.us-east-1.amazonaws.com/key`), because path-style is
deprecated there. Any remote with a custom `endpoint_url` is addressed
path-style (`http://localhost:9000/my-bucket/key`), which is what
S3-compatible servers expect. Override with `force_path_style` in
`.avc/config.local.toml` if your server disagrees.

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

Both configure and store correctly, but no adapter transfers bytes yet: `push`,
`pull`, and `list` fail with `provider adapter not implemented` and exit code
`3`. See [Roadmap](roadmap.md#1-gcs-and-azure-adapters).

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
<prefix>/objects/<algorithm>/<first-two-hash-characters>/<full-hash>
```

For `s3://my-bucket/artifacts` and hash `1dfc4d10…`:

```text
s3://my-bucket/artifacts/objects/sha256/1d/1dfc4d10…
```

Keys contain hashes only — never repository paths. A bucket shared across teams
does not reveal one team's directory structure to another. It does reveal object
sizes and the fact that two repositories reference identical content.

## Credentials

**Never put credentials in `.avc/config.toml`.** It is committed to Git. A
`region` and a `profile` name are fine there — neither authenticates anything
on its own — but a key never is.

For S3 remotes, credentials resolve in this order — first match wins:

| Order | Source | Keys |
| --- | --- | --- |
| 1 | Environment | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` |
| 2 | `.avc/config.local.toml` | `access_key_id`, `secret_access_key`, `session_token` |
| 3 | `~/.aws/credentials` | `aws_access_key_id`, `aws_secret_access_key`, `aws_session_token` for the active profile |

Provider-standard chains come first so a repository-local file can be
overridden but never silently override the environment — AVC does not become
another place secrets leak from.

Region and endpoint resolve on the same principle:

| Setting | Order |
| --- | --- |
| Region | `AWS_REGION` → `AWS_DEFAULT_REGION` → `config.local.toml` → `region` in `config.toml` → `~/.aws/config` → `us-east-1` |
| Profile | `config.local.toml` → `AWS_PROFILE` → `profile` in `config.toml` → `default` |
| Endpoint | `AWS_ENDPOINT_URL_S3` → `AWS_ENDPOINT_URL` → `config.local.toml` → `endpoint_url` in `config.toml` |

The active profile decides which section of `~/.aws/credentials` supplies keys
and which section of `~/.aws/config` supplies a region. It is the one setting
where `config.local.toml` outranks the environment: someone who wrote a profile
into a repository's local file meant *that* repository, not whatever
`AWS_PROFILE` happens to hold in the shell that ran the command.

`AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE` relocate the shared files.

Most S3-compatible servers ignore the region but still require the signature to
commit to one, which is why `us-east-1` is the final fallback rather than an
error.

### Not supported

IAM instance roles, ECS task roles, SSO, and `assume-role` are **not**
implemented. On an EC2 or ECS runner, export static credentials or inject
temporary ones (including `AWS_SESSION_TOKEN`) through the environment. See
[Roadmap](roadmap.md).

## TLS and corporate proxies

AVC verifies every HTTPS server it talks to against the Mozilla root set
compiled into the binary. On most networks that is the end of the matter and
there is nothing to configure here.

It is not the end of the matter on a network that **inspects TLS**. A corporate
proxy, a zero-trust gateway, or a scanning firewall terminates the connection,
opens it, and re-signs it with a certificate authority private to that
organization. The certificate AVC then receives is perfectly valid — it is
simply signed by a CA no public root set has ever heard of, so verification
fails:

```text
avc: HEAD https://my-bucket.s3.eu-west-1.amazonaws.com/… failed: io: invalid peer certificate: UnknownIssuer
  the server's certificate was not signed by a CA this run trusts (built-in Mozilla roots). If this
  network inspects TLS through a proxy, set AVC_SYSTEM_CERTS=1 to use the machine's own trust store,
  or point AVC_CA_BUNDLE at your organization's PEM bundle. See docs/configuration.md
```

There are two fixes. Both are machine-local and neither belongs in the tracked
`config.toml`: which certificate authorities to trust is a property of the
network a command runs on, not of the repository, and a clone taken home from
the office must not inherit the office's answer.

### Use the machine's own trust store

The simplest fix, and usually the right one. On a machine an IT department
manages, the private CA is already installed in the operating system's trust
store — that is how the browser on it works — and AVC can verify against that
store instead of its built-in roots. No file, no path:

```bash
export AVC_SYSTEM_CERTS=1
avc push
```

```toml
# .avc/config.local.toml — the same thing, permanently, for this repository
[[remotes]]
name = "origin"
use_system_certs = true
```

This reads the store the platform maintains: the system and user keychains on
macOS, the certificate stores on Windows, and the OpenSSL/ca-certificates
directories on Linux (honouring `SSL_CERT_DIR`).

### Point at a PEM bundle

When there is no system store to read — a slim container image, a CI runner —
name the file instead. Ask whoever administers the proxy for its CA in PEM form:

```bash
export AVC_CA_BUNDLE=/etc/ssl/certs/corporate-root.pem
avc push
```

```toml
# .avc/config.local.toml
[[remotes]]
name = "origin"
ca_bundle = "/etc/ssl/certs/corporate-root.pem"
```

**A bundle replaces the built-in roots rather than adding to them.** That is
what `AWS_CA_BUNDLE`, `SSL_CERT_FILE`, and `curl --cacert` all mean, and it
matters: a file holding only the corporate CA will verify the proxy and reject
everything the proxy does not sign. Most bundles handed out by an IT department
are already a full root set with the private CA appended. If yours is not, make
one:

```bash
cat /etc/ssl/certs/ca-certificates.crt corporate-root.pem > /etc/ssl/certs/avc-bundle.pem
```

The file must be PEM — the `-----BEGIN CERTIFICATE-----` format — and may hold
any number of certificates. A DER file (often named `.crt` or `.cer`) converts
with:

```bash
openssl x509 -inform der -in corporate-root.crt -out corporate-root.pem
```

The bundle is read when the command starts, not when it first connects, so a
path that does not exist or a file that is not PEM is reported as itself:

```text
avc: cannot read the CA bundle at /etc/ssl/certs/typo.pem: No such file or directory
avc: the CA bundle at /etc/ssl/certs/corporate.crt contains no certificates; it must be PEM, not
     DER — convert one with `openssl x509 -inform der -in ca.crt -out ca.pem`
```

### Resolution order

First match wins:

| Order | Source | Effect |
| --- | --- | --- |
| 1 | `AVC_CA_BUNDLE` | Trust the PEM bundle at this path |
| 2 | `AVC_SYSTEM_CERTS` (`1`, `true`, `yes`, `on`) | Trust the system store |
| 3 | `AWS_CA_BUNDLE` | Trust the PEM bundle at this path |
| 4 | `SSL_CERT_FILE` | Trust the PEM bundle at this path |
| 5 | `ca_bundle` in `config.local.toml` | Trust the PEM bundle at this path |
| 6 | `use_system_certs` in `config.local.toml` | Trust the system store |
| 7 | *(nothing set)* | Trust the built-in Mozilla roots |

`AWS_CA_BUNDLE` and `SSL_CERT_FILE` are read because a managed machine or image
usually sets one of them already, for the AWS CLI, for `curl`, or for OpenSSL —
so AVC works there without any AVC-specific setup. `AVC_CA_BUNDLE` exists to
override those for AVC alone when they point somewhere that does not suit.

### Verifying the setup

`avc list` performs one signed request and nothing else, which makes it the
cheapest way to test the configuration:

```bash
AVC_SYSTEM_CERTS=1 avc list
```

Compare with `curl`, which uses a trust store of its own and so tells you
whether the problem is the bundle or the network:

```bash
curl -sSI --cacert /etc/ssl/certs/corporate-root.pem https://my-bucket.s3.eu-west-1.amazonaws.com/
```

### What is deliberately absent

There is **no way to disable certificate verification.** No flag, no
environment variable, no configuration key. An artifact store holds
credentials-worth of trust — the bytes it returns are executed, trained on, and
shipped — and a switch that turns verification off is a switch someone
eventually leaves on in CI. If verification fails, the fix is to name the CA
that should have been trusted.

Client certificates (mutual TLS) are not implemented; open an issue if a proxy
requires one.

## Configuring AVC from the environment

A build agent has nowhere to put a config file, so the two commands built for
one — `avc fetch` and `avc verify` — read theirs from the environment:

| Variable | Equivalent flag | Applies to |
| --- | --- | --- |
| `AVC_REPO` | `--repo <git-url>` | `fetch`, `verify`, `list` |
| `AVC_REF` | `--ref <rev>` | `fetch`, `verify`, `list` |
| `AVC_CACHE_DIR` | `--cache <dir>` | `avc fetch` |
| `AVC_COLOR` | `--color <auto\|always\|never>` | every command |
| `AVC_PROGRESS` | `--progress <auto\|always\|never>` | every command |
| `AVC_CA_BUNDLE` | *(none)* | every command that reaches an `https` store |
| `AVC_SYSTEM_CERTS` | *(none)* | every command that reaches an `https` store |

Setting `AVC_REPO` and `AVC_REF` once at the top of a pipeline reduces every job
to `avc fetch <path>`. A flag on the command line always wins over the variable.

Note what is *not* in that table: there is no environment variable naming a
bucket. The object store belongs to the repository, read from the
`.avc/config.toml` at the revision being consumed, so a consumer configures
credentials and nothing else. `--remote-url` overrides it for a single run when
you genuinely need a different store — a mirror, or an air-gapped copy.

The credential and endpoint variables in the table above apply as usual; nothing
else needs to exist on disk. See [CI/CD](ci-cd.md).

## `.avc/config.local.toml`

Gitignored by `avc init`. Holds machine-local overrides such as an alternate
endpoint for a developer's local MinIO, or credentials that cannot come from a
provider chain.

Remotes are matched to `config.toml` by `name`:

```toml
[[remotes]]
name = "origin"
endpoint_url = "http://localhost:9000"
region = "us-east-1"
access_key_id = "minioadmin"
secret_access_key = "minioadmin"
# session_token = "..."       # for temporary credentials
# profile = "minio-dev"       # which ~/.aws/credentials section to read
# force_path_style = true     # override the addressing default
# ca_bundle = "/etc/ssl/certs/corporate-root.pem"  # trust this CA bundle
# use_system_certs = true     # trust the machine's own store instead
```

| Field | Meaning |
| --- | --- |
| `name` | Must match a remote in `config.toml`; other entries are ignored |
| `endpoint_url` | Replaces the tracked endpoint for this machine |
| `region` | SigV4 signing region; overrides `region` in `config.toml` |
| `access_key_id` / `secret_access_key` | Static credentials |
| `session_token` | Temporary-credential token, sent as `x-amz-security-token` |
| `profile` | `~/.aws/credentials` section to read when no key is set here; overrides `AWS_PROFILE` and `profile` in `config.toml` |
| `force_path_style` | `true` for `endpoint/bucket/key`, `false` for virtual-hosted |
| `ca_bundle` | PEM bundle of certificate authorities to verify servers against, replacing the built-in roots |
| `use_system_certs` | `true` to verify against the operating system's trust store |

A malformed `config.local.toml` is an error rather than a silent fallback:
ignoring it would mean sending a request to the wrong endpoint, or with no
credentials at all.

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
