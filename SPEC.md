# AVC Format Specification

Status: Iteration 0 prototype contract. Formats remain provisional until clone, branch, merge, push, pull, and recovery workflows run against a real repository.

## MVP Decisions

- Artifact model: regular files and directories. A directory is tracked as a single artifact whose object is a manifest of the files beneath it.
- Pointer placement: sibling file with `.avc` appended. `model.bin` uses `model.bin.avc` and `data/` uses `data.avc`; the artifact path itself remains ignored by Git.
- Hash: SHA-256 over exact file bytes, streamed in bounded memory. SHA-256 is the only algorithm AVC mints. MD5 is readable but never chosen; see *Hash Algorithms*.
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

`version` must equal `1`; `path` must be repository-relative and contain no traversal; `algorithm` must be `sha256` or `md5`; `hash` must be lowercase hexadecimal of the width that algorithm defines — 64 characters for `sha256`, 32 for `md5` — and a digest whose width does not match its declared algorithm is rejected; `size` must be present and fit `u64`. Unknown YAML fields are rejected by policy before format freeze. `media_type` is optional metadata.

An optional `kind` field follows `path` and is either `file` or `directory`. It is absent for a file, so file pointers are byte-identical to those written before directories were supported, and an absent `kind` parses as `file`.

## Directory Format

A directory pointer's `object` is not the artifact's bytes; it is a manifest object describing the directory's contents, with `media_type: application/vnd.avc.tree+yaml` and a `size` measured in manifest bytes:

```yaml
version: 1
path: data
kind: directory
object:
  algorithm: sha256
  hash: bb...
  size: 387
  media_type: application/vnd.avc.tree+yaml
```

The manifest object itself is UTF-8 YAML with LF line endings:

```yaml
version: 1
entries:
- path: a.bin
  algorithm: sha256
  hash: b6...
  size: 6
- path: nested/b.bin
  algorithm: sha256
  hash: f2...
  size: 5
```

Entry paths are relative to the tracked directory, never to the repository, so identical content tracked at two paths yields one manifest and one set of objects. Entries are sorted by `path` and must be unique: canonical order is part of the manifest's identity, and a manifest that is unsorted or repeats a path is rejected. Entry paths are validated on read exactly as a pointer's `path` is, because a manifest decides where `checkout` writes. A directory's identity is its manifest's digest, so any file added, removed, renamed, or edited beneath it changes the artifact.

A directory is stored as `1 + n` objects, and each is an ordinary immutable object: manifest objects and file objects share one keyspace, one cache, and one transport. A manifest is uploaded after the objects it names and downloaded before them, so a manifest visible on a remote never names bytes that are absent.

## Object Keys

Logical object key is `objects/<algorithm>/<first-two-hash-characters>/<full-hash>`. The algorithm is part of the key, so objects addressed by different algorithms share one keyspace without colliding, and one listing of `objects/` enumerates all of them. Object keys contain no user path. Existing valid objects are immutable.

## Hash Algorithms

An object's algorithm is part of its identity, recorded in the pointer, in every manifest entry, and in the object key. A repository may therefore hold artifacts addressed by different algorithms at once, and every operation that re-hashes content — `status`, `checkout`, `doctor`, `verify`, and download verification — uses the algorithm the pointer or manifest entry names rather than a global default.

`sha256` is the only algorithm AVC creates. `md5` exists solely so that a repository imported from a tool that used it can preserve the identities its objects already have, which is what makes an import a move rather than a full re-read of every object. MD5 is not collision-resistant; an artifact addressed by it carries weaker guarantees than the rest of the format provides, and `avc migrate dvc --rehash` exists to trade network cost for SHA-256 identities. Deduplication does not span algorithms: identical bytes addressed two ways are two objects.

## Repository Configuration

Tracked `.avc/config.toml` contains provider, bucket/container, prefix, endpoint, and remote names. Credentials never belong in tracked config. Local credential overrides belong in ignored `.avc/config.local.toml`, and provider-standard credential chains take precedence over it.

## Object Transport

All remotes are reached through one provider-neutral interface over object keys: `put`, `get`, `exists`, and `list`. Reading artifacts requires `get` alone, so a consumer may hold credentials permitting nothing else. Backends receive object identities only and never a repository path. Transfers stream in bounded memory. A download is verified against its pointer's size and digest before it becomes visible in the cache; a mismatch leaves no partial object behind.

S3 requests are signed with AWS Signature Version 4. Because object keys are content-addressed, the `x-amz-content-sha256` of a SHA-256 object's upload is the object's own digest, so payload bytes are never read twice; an object addressed by any other algorithm has no such digest to present and is sent with `UNSIGNED-PAYLOAD`, which TLS protects in transit and the caller's own verification catches on the way back out. A destination may satisfy a `put` by asking the service to copy an object it already holds, which moves no bytes through the client; the copy is verified to have succeeded, including when the service reports failure inside the body of a `200` response. Amazon S3 is addressed virtual-hosted-style; a remote with an explicit endpoint is addressed path-style unless overridden.

## Safety

All cache and worktree writes use temporary files followed by verification and atomic replacement where supported. A download is verified against its pointer before it becomes visible at whatever destination it was written to, cache or worktree alike; the cache is a convenience, not the only path bytes may take. Dirty user files are never replaced without `--force`, and that check applies per file inside a directory. Cache reads verify both size and SHA-256, including a manifest read before it is parsed. No remote deletion occurs in MVP. Materializing a directory never deletes a file the manifest does not name.

## Exit Codes

This contract reserves `0` for success, `1` for expected user/data/state errors, `2` for invalid CLI usage, and `3` for provider or operational failures.
