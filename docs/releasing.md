# Releasing

How a version of AVC becomes a tag, a GitHub release, and four binaries.

This page is for maintainers. To *install* a release, see
[Getting Started](getting-started.md#install).

## The shape of it

A release is cut by pushing an annotated `vX.Y.Z` tag to `main`. Everything
else follows from what the repository says at that tag:

- the version comes from `[workspace.package].version` in `Cargo.toml`,
- the release notes come from the `## [X.Y.Z]` section of
  [`CHANGELOG.md`](../CHANGELOG.md),
- the binaries come from building `avc-cli` at that commit.

Nothing is typed into a web form and nothing is uploaded by hand, so the tag is
the only thing a human has to get right — and
[`release.yml`](../.github/workflows/release.yml) refuses to publish if the tag
disagrees with either of the two files above.

## Versioning

AVC follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with the
`0.x` caveat that the on-disk formats are provisional: while the major version
is `0`, a **minor** release may change a format in a way that an older AVC
cannot read. See [format freeze](roadmap.md#format-freeze).

| Change | Bump |
| --- | --- |
| Bug fix, docs, dependency update with no behavior change | patch — `0.1.0` → `0.1.1` |
| New command, new flag, new remote provider | minor — `0.1.0` → `0.2.0` |
| A pointer, manifest, or object-key format an older AVC cannot read | minor, while `0.x` |
| Removing or repurposing a command, flag, or exit code | minor, while `0.x` |

A tag with a hyphen in it — `v0.2.0-rc.1` — is published as a GitHub
pre-release automatically.

## Cutting a release

Everything happens on a branch and lands through a pull request; only the tag is
pushed to `main` directly.

**1. Decide the version** using the table above, from what sits under
`## [Unreleased]` in the changelog.

**2. Move the changelog entries.** Rename `## [Unreleased]` to
`## [X.Y.Z] - YYYY-MM-DD`, add a fresh empty `## [Unreleased]` above it, and
update the two link references at the foot of the file:

```markdown
[Unreleased]: https://github.com/jvdavim/avc/compare/vX.Y.Z...HEAD
[X.Y.Z]: https://github.com/jvdavim/avc/compare/vW.V.U...vX.Y.Z
```

The section you just dated becomes the release notes verbatim, so read it as
the announcement it is about to be.

**3. Bump the version.** It is declared once, in the workspace manifest, and
both crates inherit it:

```bash
sed -i 's/^version = "0.1.0"$/version = "0.2.0"/' Cargo.toml
cargo check --workspace          # refreshes Cargo.lock
```

`Cargo.lock` records the workspace crates too, so it must be committed with the
bump or the release build's `--locked` will fail.

**4. Run the checks CI runs**, so a release does not discover a problem a
pull request could have:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo +$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^rust-version = "\(.*\)"$/\1/p' Cargo.toml) check --workspace
```

**5. Open the pull request, and merge it** once CI is green.

**6. Tag the merge commit and push the tag:**

```bash
git switch main && git pull
git tag -a v0.2.0 -m "v0.2.0"
git push origin v0.2.0
```

Pushing the tag is what starts the release. Watch it:

```bash
gh run watch --exit-status $(gh run list --workflow Release --limit 1 --json databaseId --jq '.[0].databaseId')
```

## What the workflow does

[`release.yml`](../.github/workflows/release.yml) runs three jobs.

**`verify`** checks that the tag matches the workspace version and that the
changelog has a section for it, then extracts that section into `notes.md`. A
mismatch fails here, before anything is built and before the release exists.

**`build`** compiles `avc-cli` for four targets and packages each one as a
`.tar.gz` containing the binary, `README.md`, `LICENSE`, and `CHANGELOG.md`:

| Target | Runner | Native |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | yes |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | yes |
| `aarch64-apple-darwin` | `macos-latest` | yes |
| `x86_64-apple-darwin` | `macos-latest` | cross-compiled |

Three of the four are built on a runner of their own architecture and are
smoke-tested by running `avc --version` and checking it reports the version
being released. Intel macOS is the exception: it is cross-compiled from the
Apple Silicon runner, because GitHub's Intel macOS runners are on their way out,
and so it is built but not run.

The build uses `--locked`, so the published binaries come from the exact
dependency versions CI tested rather than whatever resolved that morning.

**`publish`** collects the archives, writes a combined `SHA256SUMS`, and creates
the GitHub release with `notes.md` as its body. It is the only job with write
permission, and it is skipped on a manual run.

## Testing the workflow without spending a version

`workflow_dispatch` builds and verifies an existing tag without publishing
anything:

```bash
gh workflow run Release --ref main -f tag=v0.1.0
```

The `verify` and `build` jobs run in full and the archives land as workflow
artifacts; `publish` is skipped.

## If a release goes wrong

**The workflow failed before `publish`.** Nothing was released. Fix the problem
on a branch, merge it, then move the tag:

```bash
git tag -d v0.2.0 && git push origin :refs/tags/v0.2.0
git tag -a v0.2.0 -m "v0.2.0" && git push origin v0.2.0
```

Moving a tag is only safe because nothing consumed it yet.

**The release published and is wrong.** Do not move the tag — somebody may
already hold those bytes. Delete the release and its assets, and ship the fix as
the next patch version:

```bash
gh release delete v0.2.0 --yes
```

## Dependency updates

Dependabot opens pull requests weekly, grouped so that minor and patch Cargo
updates arrive as one, per [`dependabot.yml`](../.github/dependabot.yml). They
are ordinary pull requests: CI gates them, and a major bump that needs code
changes gets those changes in the same branch.

A dependency update that raises the workspace MSRV is a deliberate decision, not
a side effect — see [Verifying the MSRV](development.md#verifying-the-msrv). The
MSRV lives in `Cargo.toml` alone; CI reads it from there rather than pinning a
Rust version in a workflow, so there is nothing for Dependabot to misread as an
action release.

## Not yet automated

- **crates.io.** AVC is not published there. When it is, `publish` grows a
  `cargo publish` step for `avc-core` then `avc-cli`, in that order.
- **Windows binaries.** CI tests on Windows, but no `.zip` is published.
- **Signing and provenance.** The archives carry SHA-256 sums and nothing more.
