# Contributing to AVC

Thanks for your interest in contributing.

> [!WARNING]
> **AVC is vibe coded.** Nearly all of its code, tests, and documentation were
> written by AI coding assistants under human direction and review. Two things
> follow for a contributor. First, a report that the documentation describes
> behavior the code does not have is a *real bug* and one of the most useful
> things you can send. Second, human-written contributions are welcome without
> qualification — nothing here needs to be produced the way the existing code
> was.

**The full contributing guide lives at [`docs/contributing.md`](docs/contributing.md).**

## Quick start

```bash
git clone https://github.com/jvdavim/avc.git
cd avc
cargo build
cargo test --workspace
```

Before pushing, run the same four checks CI runs:

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## The short version

- **Open an issue first** for anything beyond a typo or small doc fix —
  especially for behavior changes, new dependencies, or changes to
  [`SPEC.md`](SPEC.md).
- **One logical change per pull request.**
- **Add a test** for any bug fix or new validation rule.
- **Update the docs** in `docs/` when behavior changes.
- Contributions are accepted under the MIT License. There is no CLA.

## Where to look

| | |
| --- | --- |
| Full contribution guide | [`docs/contributing.md`](docs/contributing.md) |
| Dev environment and debugging | [`docs/development.md`](docs/development.md) |
| Cutting a release | [`docs/releasing.md`](docs/releasing.md) |
| How the code is organized | [`docs/architecture.md`](docs/architecture.md) |
| What needs doing | [`docs/roadmap.md`](docs/roadmap.md) |
| Format and safety contract | [`SPEC.md`](SPEC.md) |
| Community expectations | [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) |
| Reporting a vulnerability | [`SECURITY.md`](SECURITY.md) |

Items marked **good first issue** in the [roadmap](docs/roadmap.md) are scoped to
be approachable without deep knowledge of the codebase.
