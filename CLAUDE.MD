# Repository Guidelines

## Project Structure & Module Organization

Hotaru is a Tauri 2 tray application. Rust code lives in `src-tauri/src/`: `engine.rs` runs the monitoring loop, `tray.rs` renders tray state, `windows.rs` manages webviews, and `commands.rs` exposes Tauri commands. Keep shared models in `models.rs`, preferences in `settings.rs`, and runtime state in `state.rs`.

Backend-specific code belongs in `src-tauri/src/providers/`, one module per backend (`komari.rs`, `monitor.rs` for 极简探针). A provider owns its wire format and its live session; everything a backend does not change — settings watching, reconnect backoff, error reporting, publishing, the ping cache — stays in `engine.rs`. Adding a backend should not require touching `engine.rs` beyond `providers::Provider`. Anything the popover renders must arrive as a `NodeSnapshot` field, so a difference in meaning between backends (traffic quotas being lifetime on one and monthly on the other, say) is resolved in the provider rather than in JavaScript.

The build-free frontend is in `ui/` (`index.html` and `chart.html`). Capabilities and packaging configuration live in `src-tauri/capabilities/` and `src-tauri/tauri.conf.json`; icons are under `src-tauri/icons/`. `tools/mock-komari/` is a standalone mock server; `docs/` contains previews.

## Build, Test, and Development Commands

- `cargo install tauri-cli --locked --version "^2"` installs the expected Tauri CLI.
- `cargo tauri dev` runs the desktop app from the repository root with live rebuilds.
- `cargo tauri build` creates platform installers under `src-tauri/target/release/bundle/`.
- `tools/strip-dmg-volume-icon.sh <dmg>` removes the stray `.VolumeIcon.icns` Tauri leaves inside a macOS dmg; run it after `cargo tauri build` (the release workflow does this on its own).
- `cargo test --manifest-path src-tauri/Cargo.toml` runs the Rust unit tests.
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` verifies formatting.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` catches Rust lint issues.

## Coding Style & Naming Conventions

Use standard `rustfmt` formatting (four-space indentation) and idiomatic Rust naming: `snake_case` for modules and functions, `PascalCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Keep platform-specific behavior behind `#[cfg(target_os = "...")]`. In HTML/CSS/JavaScript, follow the existing two-space indentation, use semantic class names, and reuse CSS custom properties.

## Cross-Platform & UI Requirements

Every feature must support both macOS and Windows. Isolate unavoidable differences with targeted `cfg` blocks; do not assume macOS-only APIs, window behavior, tray features, paths, or shortcuts. Test both platforms when practical, or identify the unverified platform in the pull request.

Use macOS as the primary visual direction: restrained spacing, rounded surfaces, subtle borders, and native-feeling interactions. Preserve Windows usability where behavior differs. Reuse theme variables and support light and dark modes.

## Testing Guidelines

Tests use Rust's built-in `#[test]` framework in colocated `#[cfg(test)] mod tests` blocks. Add focused tests beside changed logic and use descriptive names such as `aggregates_offline_nodes`. Bug fixes should include a regression test when practical. Run formatting, Clippy, and all tests before submitting.

## Commit & Pull Request Guidelines

Recent history primarily follows Conventional Commit prefixes such as `feat:`, `fix:`, `chore:`, `docs:`, and `refactor:`. Write an imperative, narrowly scoped subject; release commits may use `Release vX.Y.Z`.

Pull requests should explain the user-visible change, affected platforms, and verification performed. Link issues and include screenshots for UI changes. Call out changes to capabilities, signing, authentication, or packaging; never commit API keys or private backend URLs.
