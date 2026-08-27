## Summary

<!-- What does this change and why? Link the issue it addresses. -->

Closes #

## Type of change

- [ ] Bug fix
- [ ] New feature
- [ ] Documentation
- [ ] Refactor (no behavior change)
- [ ] Change to `SPEC.md` (format or safety contract)

## Behavior change

<!-- If command behavior or output changed, show before and after. Delete if not applicable. -->

## Checklist

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo check --workspace` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] Tests added for the change (a bug fix should have a test that fails without it)
- [ ] Docs in `docs/` updated if behavior changed
- [ ] `SPEC.md` updated if a format or safety rule changed
- [ ] The [invariants](../docs/architecture.md#invariants-to-preserve) still hold

## Notes for reviewers

<!-- Anything that would help review: tricky cases, alternatives considered, known gaps. -->
