# Agentlaw contributor guidance

This is the Rust implementation repository, not the former target-project seed.
Do not reinstall old governance files, run legacy Python initialization, or treat
older tags as the current runtime contract.

- Start with `README.md`, `docs/usage.md` and the crate READMEs.
- `docs/design/contracts/agentlaw-tool.schema.json` is the public typed tool surface;
  use `action` plus its same-named typed input object. Legacy flat calls are
  normalized internally, not advertised as a second model-facing format.
  `agentlaw-input.schema.json` in that directory and its examples define runtime
  validation. Keep both aligned without exposing conditional validation as the
  model's input description. `docs/contracts/agentlaw-llm-guidance.md` contains
  the accepted tool/bootstrap guidance compiled into the product.
- Preserve complete Markdown memory, history and pending proposals. Runtime
  validates mechanical conditions; it must not invent semantic merge decisions.
- Tests use isolated stores and process handles they own. Do not modify an active
  harness, real user memory, or unrelated processes to run a test.
- Use `cargo fmt --all -- --check` and `cargo test --locked --workspace --no-fail-fast`.
  Real-model tests are opt-in. Report actual coverage, not blanket correctness.
- Keep ignored artifacts, credentials, machine-local settings and memory out of Git.
- Respect user authorization for commits, pushes, installation and release actions.
