## Summary

<!-- What does this change do, and what problem does it solve? Link the issue
it resolves with a closing keyword, e.g. `Closes #123`. -->

## What's in the diff

<!-- A bullet per file or logical unit. Call out anything a reviewer should
look at first. -->

## Test plan

<!-- How you verified the change. -->

- [ ] `cargo clippy --all-targets --all-features -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] `cargo test` / `cargo nextest run` pass for the affected crates
- [ ] Manual verification (describe, if applicable)

## Checklist

- [ ] Commit subjects and the PR title follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] Documentation updated where behavior changed
- [ ] No secrets, credentials, or partner-specific names in the diff
