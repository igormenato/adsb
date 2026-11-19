# trck

## Change

Smallest change that does the ticket. Subtract before you add. No dead code, no speculative abstractions. No `unwrap`/`expect` outside tests; propagate errors with `?` and anyhow context like existing code. No `unsafe` and no new dependency unless the PR says why. Every bug fix ships a test that fails before and passes after.

## Working

While iterating, run filtered quiet tests (`cargo test --locked -q --test <file> <name>` or `--lib <name>`).
Run the full gate once before opening/updating the PR:
`cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked -q`.
Grep with a path and a narrow pattern. Read the function you need, not whole files.
Don't re-read a file after your own edit (check with a filtered test or `git diff` of that hunk).
No review subagents unless the brief asks.

## Cargo

Pi binary: install musl-tools, then on an aarch64 host:
`cargo build --release --locked --target aarch64-unknown-linux-musl`
Copy `target/aarch64-unknown-linux-musl/release/trck`. The release asset is `trck-aarch64`. On x86, use `cross` or a similar toolchain.

## PR / CI

Put the gate output in the PR body.
For CI, subscribe once; don't poll `gh pr checks --watch`; don't fetch full PR JSON.

## Release

A push to `main` opens or updates a release PR. Merging it tags `v` plus the crate version and attaches `trck-aarch64`. Do not push tags by hand.
