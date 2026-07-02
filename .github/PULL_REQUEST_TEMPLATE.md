## What & why



## Checklist
- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo xtask graph-check` passes
- [ ] `cargo nextest run --workspace --no-tests=pass` passes locally
- [ ] CI green on all 6 targets

## Contract impact
- [ ] Touches a contract crate (`midir` / `rt-abi`) — describe the IR/ABI change and who it unblocks
- [ ] Adds/removes an internal dependency edge — `graph-allowlist.toml` updated and justified
- [ ] Corpus / `.mir` fixtures added or changed

## Time tracking
- [ ] Logged my active time this session (`scripts/timelog.sh in/switch/out`) — see CLAUDE.md
