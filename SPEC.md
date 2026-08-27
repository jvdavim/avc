# AVC Format Specification

Status: Iteration 0 prototype contract. Formats remain provisional until clone, branch, merge, push, pull, and recovery workflows run against a real repository.

## MVP Decisions

- Artifact model: independent regular files only. Directories are rejected.
- Pointer placement: sibling file with `.avc` appended. `model.bin` uses `model.bin.avc`; the artifact path itself remains ignored by Git.
- Hash: SHA-256 over exact file bytes, streamed in bounded memory.
- Minimum Rust: 1.75. Minimum Git: 2.30. Intended OSes: macOS, Linux, and Windows.
- Remote providers: explicit `s3://`, `s3+https://`, `s3+http://`, `gs://`, and `az://` schemes. `file://` is supported as an offline development remote. Provider is never inferred from arbitrary hostnames.

## Pointer Format

Pointer files use UTF-8 YAML, LF line endings, no timestamps, and this field order:

```yaml
version: 1
path: model.bin
object:
  algorithm: sha256
  hash: 7f...
  size: 4294967296
  media_type: application/octet-stream
```

`version` must equal `1`; `path` must be repository-relative and contain no traversal; `algorithm` must equal `sha256`; `hash` must be 64 lowercase hexadecimal characters; `size` must be present and fit `u64`. Unknown YAML fields are rejected by policy before format freeze. `media_type` is optional metadata.

## Object Keys

Logical object key is `objects/sha256/<first-two-hash-characters>/<full-hash>`. Object keys contain no user path. Existing valid objects are immutable.

## Repository Configuration

Tracked `.avc/config.toml` contains provider, bucket/container, prefix, endpoint, and remote names. Credentials never belong in tracked config. Local credential overrides belong in ignored `.avc/config.local.toml`, and provider-standard credential chains take precedence over it.

## Object Transport

All remotes are reached through one provider-neutral interface over object keys: `put`, `get`, `exists`, and `list`. Backends receive object identities only and never a repository path. Transfers stream in bounded memory. A download is verified against its pointer's size and digest before it becomes visible in the cache; a mismatch leaves no partial object behind.

S3 requests are signed with AWS Signature Version 4. Because object keys are content-addressed, the `x-amz-content-sha256` of an upload is the object's own digest, so payload bytes are never read twice. Amazon S3 is addressed virtual-hosted-style; a remote with an explicit endpoint is addressed path-style unless overridden.

## Safety

All cache and worktree writes use temporary files followed by verification and atomic replacement where supported. Dirty user files are never replaced without `--force`. Cache reads verify both size and SHA-256. No remote deletion occurs in MVP.

## Exit Codes

This contract reserves `0` for success, `1` for expected user/data/state errors, `2` for invalid CLI usage, and `3` for provider or operational failures.
