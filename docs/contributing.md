# Contributing to AVC

Thanks for considering a contribution. AVC is a young project, which means the
barrier to making a real difference is low and almost every part of it is still
open to discussion.

By participating you agree to abide by the
[Code of Conduct](../CODE_OF_CONDUCT.md).

## Ways to contribute

You do not need to write Rust to help.

- **Report a bug.** Especially anything involving data loss, a corrupted cache, or
  a pointer AVC refuses to read. Those are the highest-priority reports.
- **Improve the docs.** If something in `docs/` was wrong, stale, or confusing,
  fixing it is a genuinely useful contribution.
- **Try it and report friction.** AVC has few users. A plain description of what
  you expected versus what happened is valuable data.
- **Review a pull request.** A second pair of eyes on unsafe path handling or
  filesystem writes is always welcome.
- **Write code.** See [Roadmap](roadmap.md) — items marked **good first issue**
  are scoped to be approachable.

## Before you start

**Open an issue first for anything non-trivial.** A typo fix or a small doc
correction can go straight to a pull request. For a behavior change, a new
command, a new dependency, or anything touching `SPEC.md`, please discuss it
first — it is much cheaper to redirect an approach in an issue than to reject a
finished branch.

Changes that **require** discussion before code:

- Any change to the pointer format or `SPEC.md`
- Adding a dependency
- Changing an existing command's behavior or output
- Anything that weakens one of the [invariants](architecture.md#invariants-to-preserve)

## Development setup

See [Development](development.md) for the full environment guide. The short
version:

```bash
git clone https://github.com/jvdavim/avc.git
cd avc
cargo build
cargo test --workspace
```

## The checks

Run all four before pushing. CI runs exactly these, so a green local run means a
green CI run:

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Clippy warnings are errors. This is intentional — it keeps review focused on
design rather than style.

## Making a change

1. **Fork and branch.** Branch from `main` with a descriptive name:
   `s3-adapter`, `fix-gc-reachability`, `docs-configuration`.
2. **Keep it focused.** One logical change per pull request. A refactor bundled
   with a behavior change is hard to review and hard to revert.
3. **Add a test.** Any bug fix should come with a test that fails without it. Any
   new validation rule needs a test for the case it rejects.
4. **Update the docs.** If you change behavior, update the page in `docs/` that
   describes it. If you change a format rule, update `SPEC.md` in the same pull
   request.
5. **Run the checks.** All four, above.

### Testing conventions

Unit tests live at the bottom of `crates/avc-core/src/lib.rs`. When adding one:

- Assert on **exact** expected output for anything format-related. The existing
  `pointer_serialization_is_stable_and_round_trips` test asserts the precise YAML
  string, which is what makes accidental field reordering a test failure rather
  than a silent format break.
- Test the rejection path, not just the happy path. Path validation and pointer
  parsing are security boundaries; the interesting cases are the ones that should
  fail.

CLI-level behaviour is tested by driving the binary itself, in
`crates/avc-cli/tests/directory.rs` (directory artifacts) and
`crates/avc-cli/tests/ci.rs` (`fetch` and `verify`). Assert against
`--porcelain` output, not the human-facing tables — the former is a stable
interface and the latter is deliberately not. The single-file workflows are
still uncovered; extending the harness to them would be a welcome
contribution.

### Code style

- `cargo fmt` decides formatting. Do not hand-format.
- Match the surrounding code. It favors small functions, early returns, and
  explicit error strings over abstraction.
- Errors in `avc-core` are `Error` variants; errors in `avc-cli` are `String`
  messages formatted for a human reading a terminal.
- Error messages are lowercase, specific, and say what to do next where possible:
  `AVC is not initialized; run 'avc init'` rather than `not initialized`.
- Every write to the cache or worktree goes through `write_atomic` or
  `copy_atomic`. No exceptions — this is the mechanism behind the SPEC's
  atomicity guarantee.

## Pull requests

- **Title:** short and imperative — `Add S3 provider adapter`, not
  `added s3 stuff`.
- **Description:** what changed and why. Link the issue it addresses. If behavior
  changed, show before-and-after output.
- **Draft PRs are welcome** for early feedback on an approach.
- CI must pass. Maintainers may push small fixups to your branch to get it green.

Commits are squashed on merge, so intermediate commit messages need not be
pristine — but a clear history helps review.

### What review looks for

In rough priority order:

1. **Correctness under failure.** What happens on a partial write, a full disk,
   an interrupted transfer, a hostile pointer file?
2. **Invariants preserved.** See
   [Architecture](architecture.md#invariants-to-preserve).
3. **Tests** that would catch a regression.
4. **Documentation** that matches the new behavior.
5. Style, which `fmt` and `clippy` have mostly settled already.

Review is a conversation, not a gate. If a comment does not make sense, push
back — the reviewer may be missing context you have.

## Reporting bugs

Open an issue with:

- What you ran and what happened, including the exact error text
- What you expected
- `avc --version`, `rustc --version`, `git --version`, and your OS
- A minimal reproduction if you can produce one

For anything involving data loss or cache corruption, include the output of
`avc doctor` and `avc status`.

**Do not open a public issue for a security vulnerability.** See
[SECURITY.md](../SECURITY.md).

## Requesting features

Describe the problem before the solution. "I need to check out the artifact as of
an old commit, and today I have to do X" is more useful than "add a `--rev`
flag" — it leaves room for a better design.

Check the [Roadmap](roadmap.md) first; it may already be there, in which case a
comment saying you need it helps prioritize.

## Changing the specification

`SPEC.md` is the normative contract for on-disk formats. Changing it is a bigger
deal than changing code:

1. Open an issue describing the problem and the proposed rule change.
2. Say explicitly whether it is backward compatible with `version: 1` pointers.
3. If it is not, describe the migration path.
4. Once agreed, change `SPEC.md`, the implementation, and the tests together in
   one pull request.

The format is provisional precisely so that it *can* change during Iteration 0 —
but it should change deliberately, in the open.

## Licensing of contributions

AVC is licensed under the MIT License. Contributions are accepted under the same
license. There is no CLA — submitting a pull request is taken as agreement that
your contribution may be distributed under the MIT License.

## Getting help

- **GitHub Issues** — bugs and feature requests
- **GitHub Discussions** — questions, ideas, and design conversations

A question that turns out to be a documentation gap is a bug report. Ask it.
