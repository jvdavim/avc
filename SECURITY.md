# Security Policy

## Supported versions

AVC is a `0.x` prototype. Only the latest release receives security fixes.

| Version | Supported |
| --- | --- |
| 0.1.x | Yes |
| < 0.1 | No |

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Report privately through
[GitHub Security Advisories](https://github.com/jvdavim/avc/security/advisories/new),
which allows a fix to be prepared before details become public.

Please include:

- The type of issue and the component affected
- Full paths of the relevant source files
- Steps to reproduce, ideally with a minimal proof of concept
- The impact, including how an attacker might exploit it
- Your assessment of severity, if you have one

### What to expect

- **Acknowledgement** within 7 days.
- **Initial assessment** within 14 days, including whether it is accepted and a
  rough remediation timeline.
- **Disclosure** coordinated with you. AVC is a small project without a fixed
  embargo period; the aim is to publish an advisory once a fix is available.

Reporters are credited in the advisory unless they prefer otherwise.

## Threat model

AVC handles input that may be attacker-controlled. Cloning an untrusted
repository means parsing pointer files written by someone else, and those
pointers drive filesystem writes. The following are treated as security-relevant:

- **Path traversal.** A pointer whose `path` escapes the repository root, via
  `..`, an absolute path, a backslash, or a Windows drive prefix. Validation
  lives in `crates/avc-core/src/path.rs`.
- **Content substitution.** Any path by which bytes not matching a pointer's
  SHA-256 are accepted as valid.
- **Unintended overwrite.** Any path by which `checkout` or `pull` replaces a
  modified working file without `--force`.
- **Credential leakage.** Credentials written to tracked configuration, logged,
  or sent to an endpoint other than the configured provider.
- **Path disclosure to remotes.** Object keys must contain hashes only. A key
  that leaks repository structure to a shared bucket is a bug.
- **Resource exhaustion.** Input that causes unbounded memory use — hashing is
  streamed specifically to prevent this.

## Not vulnerabilities

- **SHA-256 collisions.** A theoretical collision is not an actionable report
  against this project.
- **`--force` overwriting local changes.** Documented behavior of an explicit
  flag.
- **`avc gc` deleting objects referenced only by other branches.** A known
  limitation, documented in [`docs/cli.md`](docs/cli.md#avc-gc) and tracked on
  the [roadmap](docs/roadmap.md). Reports of *additional* data-loss paths are
  welcome as regular issues.
- **Missing cloud authentication.** No cloud adapter is implemented in `0.1.0`.
- **Trusting a repository you chose to clone**, beyond the traversal and
  substitution cases above.

## Scope

This policy covers the `avc-core` and `avc-cli` crates in this repository.
Vulnerabilities in dependencies should be reported upstream; if a dependency
advisory affects AVC, please open an issue so the version can be bumped.
