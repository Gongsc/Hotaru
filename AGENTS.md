# Repository Guidelines

## Project Structure & Module Organization

Hotaru is a Tauri 2 tray application. Rust code lives in `src-tauri/src/`: `monitor.rs` handles Komari data, `tray.rs` renders tray state, `windows.rs` manages webviews, and `commands.rs` exposes Tauri commands. Keep shared models in `models.rs`, preferences in `settings.rs`, and runtime state in `state.rs`.

The build-free frontend is in `ui/` (`index.html` and `chart.html`). Capabilities and packaging configuration live in `src-tauri/capabilities/` and `src-tauri/tauri.conf.json`; icons are under `src-tauri/icons/`. `tools/mock-komari/` is a standalone mock server; `docs/` contains previews.

## Build, Test, and Development Commands

- `cargo install tauri-cli --locked --version "^2"` installs the expected Tauri CLI.
- `cargo tauri dev` runs the desktop app from the repository root with live rebuilds.
- `cargo tauri build` creates platform installers under `src-tauri/target/release/bundle/`.
- `tools/strip-dmg-volume-icon.sh <dmg>` removes the stray `.VolumeIcon.icns` Tauri leaves inside a macOS dmg; run it after `cargo tauri build` (the release workflow does this on its own).
- `cargo test --manifest-path src-tauri/Cargo.toml` runs the Rust unit tests.
- `node tools/test-chart.cjs` checks chart script syntax, shared axes, canvas resizing, and content-driven panel height.
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` verifies formatting.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings` catches Rust lint issues.

## Coding Style & Naming Conventions

Use standard `rustfmt` formatting (four-space indentation) and idiomatic Rust naming: `snake_case` for modules and functions, `PascalCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Keep platform-specific behavior behind `#[cfg(target_os = "...")]`. In HTML/CSS/JavaScript, follow the existing two-space indentation, use semantic class names, and reuse CSS custom properties.

## Cross-Platform & UI Requirements

Every feature must support both macOS and Windows. Isolate unavoidable differences with targeted `cfg` blocks; do not assume macOS-only APIs, window behavior, tray features, paths, or shortcuts. Test both platforms when practical, or identify the unverified platform in the pull request.

Use macOS as the primary visual direction: restrained spacing, rounded surfaces, subtle borders, and native-feeling interactions. Preserve Windows usability where behavior differs. Reuse theme variables and support light and dark modes.

### Tray Panel and Window Behavior

- The left-click tray panel (`chart`), settings window (`settings`), and main dashboard (`main`) have distinct sizing rules. Settings and main restore their size and position across opens and app restarts. The tray panel restores its width and position; its height is always determined by current content and available screen space.
- Allow manual resizing of the tray panel only horizontally. Expanding nodes grows the panel while space is available; only after reaching the monitor's available height should the window stop growing and the node list scroll. Collapsing or filtering content should shrink it again. Preserve the user's width during automatic height changes.
- Calculate available height using the relevant monitor's work area, excluding the menu bar, Dock, or taskbar. Preserve growth direction relative to the tray anchor, account for DPI and multiple monitors, and keep restored windows reachable when displays change.
- Keep geometry persistence separate from preference forms (currently `window_geometry.rs`), so saving settings cannot overwrite recent window movement. Restored geometry must never bypass content-driven height calculation.
- Treat “keep open on focus loss” and “always on top” as separate controls. The persistent keep-open setting prevents dismissal without forcing native always-on-top behavior. The pin button controls native always-on-top state and also prevents focus-loss dismissal while pinned. Unpinning removes always-on-top; dismissal then follows the keep-open setting. Default to unpinned and automatic dismissal, and recheck both states after any delayed blur handler.

### Traffic Chart Layout

- Aggregate and node traffic charts must adapt to their container width and redraw their canvas backing buffers at the current device-pixel ratio after resizing. Avoid fixed pixel widths that prevent horizontal expansion.
- Keep chart heights stable as the panel expands. Do not derive chart height from viewport height, which creates feedback between content measurement and automatic window height.
- Use a shared time window, vertical range, tick count, and label formatting for aggregate and node traffic charts. Compute the common vertical range from in-range samples across the aggregate and node series, so equal values remain comparable; exclude out-of-range samples.

## Testing Guidelines

Tests use Rust's built-in `#[test]` framework in colocated `#[cfg(test)] mod tests` blocks. Add focused tests beside changed logic and use descriptive names such as `aggregates_offline_nodes`. Bug fixes should include a regression test when practical. Run formatting, Clippy, and all tests before submitting.

For window or chart changes, also run `node tools/test-chart.cjs`. Cover content expansion below the screen limit, scrolling above it, collapse, width preservation, shared chart axes, and high-DPI canvas resizing. Native verification should check pinning independently from keep-open, horizontal-only manual resizing, and geometry restoration after restart. Report untested platforms and existing check failures explicitly; automated logic tests do not establish native window behavior.

## Commit & Pull Request Guidelines

Recent history primarily follows Conventional Commit prefixes such as `feat:`, `fix:`, `chore:`, `docs:`, and `refactor:`. Write an imperative, narrowly scoped subject; release commits may use `Release vX.Y.Z`.

Pull requests should explain the user-visible change, affected platforms, and verification performed. Link issues and include screenshots for UI changes. Call out changes to capabilities, signing, authentication, or packaging; never commit API keys or private backend URLs.
