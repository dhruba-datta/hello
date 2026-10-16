# Hello — Agent Guide

Hello is a native macOS dictation and meeting-notes app in Rust (GPUI for the UI, Metal for the
recording indicator). Read this before changing behaviour.

## Principles

- **On-device only.** No cloud services, paid APIs, accounts, telemetry or AI rewriting. The
  only network use is downloading speech models the user picks.
- **Keep behaviour working.** Add features in focused modules and keep existing tests passing.
- **Settings and data** live in `~/Library/Application Support/voice-control` (settings,
  history, models, meetings, logs). Never delete it. New settings fields need
  `#[serde(default)]` so older settings files keep loading.
- **Stable signing.** Package with `package-hello.sh`; sign with one stable certificate so
  macOS privacy permissions survive rebuilds.
- **Plain UI text.** Short, clear sentences. The UI is dark and opaque.

## Architecture

| Area | Modules |
|------|---------|
| App shell | `main` (CLI and startup), `app_window` (window and all panes), `desktop_ui` (colours, controls), `status_item`, `login_item`, `onboarding`, `permission_guide` + `native/permission_guide.m` |
| Settings | `app_settings` (persisted settings and runtime switches), `app_paths`, `config` |
| Audio and shortcuts | `audio`, `dictation_audio`, `suppression` (event tap and hotkey state machine), `keyboard`, `recording_environment`, `microphone_activity` |
| Dictation pipeline | `recognition` (listener), `dictation` (capture and resampling), `parakeet` (model runtime, language routing, Bangla + English mixing), `transcription*` (catalog, downloads, service), `gguf_session`, `apple_speech`, `moonshine` |
| Text | `smart_editing`, `dictation_processor` (Modes: replacements and transformations), `text_replacements`, `spoken_text`, `paste`, `context` |
| Indicator | `dictation_indicator` + `dictation_indicator.metal` (Rust and Metal uniform layouts must match) |
| Notetaker | `meeting`, `meeting_watcher`, `meeting_detection`, `meeting_live`, `meeting_context`, `calendar`, `speakers`, `accessibility` |
| Voice Commands | `commands`, `command_grammar`, `personal_commands` + `sdk/commands` (TypeScript kit, run with Bun) |
| History and stats | `history`, `events`, `language_samples` |
| Developer tools | `local_api`, `developer_control`, `dashboard`, `dictation_diagnostics`, `transcription_benchmark`, `moonshine_lab` |

## Behaviour to preserve

- Dictation starts on the configured shortcut (Option by default), supports double-tap lock,
  and pastes into the focused app. Paste-last has its own shortcut.
- Smart editing applies to pasted and sent dictation only, before Mode replacements.
- Bangla + English: Auto-detect cuts recordings into 1.5–3 s pieces at quiet moments, detects
  each piece's language with Whisper base, and routes spans to the English or Bangla model.
  Non-English detections count as Bangla when Bangla is the second language.
- Voice Commands and Help tune Bangla are opt-in and default off.
- Meeting recordings, speaker voices and calendar data stay on the Mac. Deleting a meeting moves
  it to the Trash.

## Build and test

```sh
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer MACOSX_DEPLOYMENT_TARGET=15.0
node sdk/commands/build.mjs
cargo build --release
cargo fmt
cargo test --release
```

Ignored tests that need real models or audio are marked `#[ignore]` with the environment
variables they read. Run them with `cargo test --release <name> -- --ignored --nocapture`.
