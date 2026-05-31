# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`native-devtools-mcp` is a Rust MCP (Model Context Protocol) server that gives MCP clients computer-use control over native desktop apps (macOS / Windows), Chrome/Electron via CDP, and Android via ADB. It speaks JSON-RPC 2.0 over stdio.

For the **agent-facing tool reference** (every tool's intent, schema, and usage patterns) see [`AGENTS.md`](./AGENTS.md) — do not duplicate that here. This file is for *developing* the server.

## Commands

```bash
# Build (requires Rust 1.91+ — adb_client needs it; .tool-versions pins it for asdf users)
cargo build --release            # binary: ./target/release/native-devtools-mcp

# Run as the MCP server (stdio transport; logs to stderr, protocol on stdout)
cargo run

# CLI subcommands (handled before the async server starts, see src/cli + src/main.rs)
cargo run -- setup               # permission checks + MCP client config wizard
cargo run -- verify              # SHA-256 the binary against published checksums

# Tests
cargo test                       # all
cargo test --lib                 # unit tests only (fast, no devices/apps needed)
cargo test --lib <substring>     # a single unit test by name
cargo test --test ax_dispatch_tests          # one integration test file (see tests/)
cargo test --test android_smoke_tests -- --ignored --test-threads=1   # real-device smoke tests (ignored by default)

cargo clippy --all-targets       # lint (CI-enforced; keep it clean)
cargo fmt                        # format
cargo bench                      # find_image benchmarks (benches/)
```

Several integration suites and `#[ignore]`d tests need a live machine/device (GUI focus, a connected Android device, or a debug Chrome) and won't pass headless.

## Architecture (big picture)

**Platform abstraction via a `platform` alias.** `main.rs` aliases the OS module — `use macos as platform` / `use windows as platform`. Tool code calls `crate::platform::{window, input, ocr, screenshot, display, ...}`, so tool logic is platform-agnostic and every OS-specific detail lives behind the alias in `src/macos/` or `src/windows/`. When adding a capability, add it to *both* platform modules with the same function surface, then call it through `platform::`.

**Tool system (`src/tools/registry.rs` + `src/server.rs`).** Each tool is a `ToolHandler` (`name()` / `schema()` / `call(args, ctx)`). `registry.rs` assembles the full tool list; `server.rs` (`MacOSDevToolsServer`, an rmcp `ServerHandler`) lists them with MCP safety annotations (readOnly/destructive/idempotent/openWorld) and routes `call_tool`. Shared state (caches, the CDP client, the AX session, the Android device) lives on the server and reaches tools through `ToolContext`. Conditional groups — `android_*`, `cdp_*` — are always listed; CDP/Android calls return a "not connected" error until their connect tool succeeds (connect/disconnect deliberately do **not** fire `tool_list_changed`, to keep prompt caches stable).

**Four interaction approaches** (see README / AGENTS.md for when to use which): Visual (screenshot+OCR+coordinate click), macOS **AX dispatch** (`src/macos/ax/` — focus-preserving, the preferred path for native macOS apps), **CDP** (`src/cdp/`), and AppDebugKit (`src/app_protocol/`).

**macOS AX seam (`src/macos/ax/`).** Reads go through a typed `attr` seam; `tree` walks the AX tree; `find` does text/point lookup; dispatch does `AXPress` / `kAXValueAttribute` / `AXSelectedRows`. Snapshots hand out generation-stamped UIDs (`a42g3`) that any new snapshot invalidates.

**CDP (`src/cdp/`, feature `cdp`).** `CdpClient` (chromiumoxide) holds the browser + selected page; `connect(port)` resolves the WebSocket from `http://127.0.0.1:{port}` and polls for a real (non-extension) page. `launch.rs` adds `cdp_launch` — spawns Chrome against a stable profile and connects. Handlers live in `cdp/tools/`.

**Feature flags (`Cargo.toml`).** Default = `find_image_fast` (SIMD + rayon) and `cdp`. The crate must build with `--no-default-features` too; keep `#[cfg(feature = ...)]` gating correct.

## Project-specific gotchas

These are non-obvious and have cost real debugging time:

- **macOS permissions are granted to the *host process*, not the binary.** Accessibility + Screen Recording must be enabled for whatever launches the server (Claude Code, Terminal, Claude Desktop). Without them, clicks/typing silently no-op and screenshots are black.
- **`type_text` and IMEs (`src/macos/input.rs`).** Physical key events route through the active input method, so with a Chinese/Japanese IME active, latin keystrokes become pinyin/composition, not literal text. `type_text` therefore temporarily switches to an ASCII-capable keyboard layout (Carbon TIS) for the keystrokes and restores the previous source via an RAII guard. All characters are injected with `CGEventKeyboardSetUnicodeString`. `press_key` is separate and stays keycode-based.
- **CDP needs a debug-enabled browser.** Chrome must be started with `--remote-debugging-port` **and** a non-default `--user-data-dir` (Chrome 136+ refuses the port on the default profile); you cannot enable debugging on an already-running normal Chrome. `cdp_launch` manages a persistent dedicated profile (`~/.native-devtools-mcp/`) so logins survive; `cdp_connect` attaches to a browser you launched yourself.
- **A localhost HTTP proxy can break CDP connect.** `CdpClient::connect` fetches `http://127.0.0.1:{port}` to resolve the WebSocket; if the server inherits an `http_proxy` env var, the proxy may intercept localhost. Not a bug in deployment (clients bypass localhost) but set `NO_PROXY=127.0.0.1,localhost` when running the server from a proxied shell.
- **`find_windows_by_app` returns windows sorted main-first** (on-screen before off-screen, then by descending area); callers take `windows[0]`. Don't reintroduce "first raw window" behavior — multi-window apps (e.g. WeChat) expose tiny off-screen panels first.
- **`find_text` is AX-first, OCR-fallback.** Accessibility-opaque apps (custom-drawn UIs) expose only their menu bar to AX; for those, fall back to `take_screenshot(include_ocr=true)` + click by coordinates. OCR is configured for multilingual (Latin + CJK) recognition.

## Verifying changes

There's no headless test for GUI / IME / CDP behavior — unit tests cover pure logic only. To verify those paths, run the built binary as a stdio MCP server and drive it with JSON-RPC (`initialize` → `notifications/initialized` → `tools/call`), then observe the real app/screen. "Tests pass" is necessary but not sufficient for input/screenshot/CDP changes — exercise them against a live target.

## Reference docs

- [`AGENTS.md`](./AGENTS.md) — full tool reference + reasoning patterns (agent-facing).
- [`CDP_PARITY.md`](./CDP_PARITY.md) — CDP feature coverage vs. Playwright.
- [`SECURITY_AUDIT.md`](./SECURITY_AUDIT.md) — which permissions are used and where.
- `examples/` — task recipes (AX dispatch flow, OCR fallback, template matching, Android quickstart).
