# Contributing to Hello

Thanks for your interest in Hello. Read [`AGENTS.md`](AGENTS.md) for how the app is organized
and the rules every change follows.

## Ground rules

- Everything stays **free and on-device**: no cloud services, paid APIs, accounts or
  telemetry.
- Keep existing behaviour working. Prefer focused modules for new features.
- Match the surrounding code style. Run `cargo fmt` and `cargo test --release` before sending a
  change.

## Set up

Install Xcode (with the Metal toolchain), `cmake`, Rust and Node.js, then follow
[Building from source](README.md#building-from-source) in the README.

## Pull requests

Describe what changed and how you checked it: tests, and for UI changes, a screenshot of the
affected pane.
