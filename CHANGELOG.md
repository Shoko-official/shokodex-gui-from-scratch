# Changelog

All notable changes to **Shokodex** will be documented in this file.

## [0.1.0] - 2026-06-06

### Added
- Interactive REPL terminal session that maintains conversation history.
- Support for single-query runs via CLI arguments (e.g., `cargo run "query"`).
- Fallback auto-detection of a loaded model from LM Studio if `LMSTUDIO_MODEL` is unset.
- Custom Unicode border framing for the startup card and input prompt.
- Temporary "Thinking..." status printing during request flights.
- Automatic rollback of failed user messages from history to prevent conversation corruption.
- Added dependencies: `anyhow`, `reqwest`, `serde`, and `tokio`.
- Updated Rust edition to `2021` in `Cargo.toml`.
- Added `.gitignore` configuration for `Cargo.lock`.
- Created `Citations.md` listing project dependencies and technical inspirations.
