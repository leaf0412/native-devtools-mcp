# End-to-End Smoke Tests (manual, live machine)

Unit tests (`cargo test`) cover pure logic only. GUI / input / screenshot /
IME / CDP behaviour has **no headless test** — it can only be verified by
driving the built binary against a real desktop. This directory is that:
a JSON-RPC driver plus a coverage map, scenario backlog, and a failure-mode log.

**Why it exists (the point):** so testing *accumulates*. Each session updates
the coverage map and the failure-mode log instead of re-discovering the same
gaps. We find problems by proactively exercising scenarios, not by waiting for
them to break in production. When you fix a "chokepoint", you grep for the
parallel paths and add the missing scenario here so it can't silently regress.

## Running

```bash
cargo build --release          # DEFAULT features (a --no-default-features binary has no cdp_* tools)
python3 scripts/e2e/macos_smoke.py
```

Prereqs: a macOS GUI session (not SSH/headless); **Accessibility + Screen
Recording granted to the host process** (the terminal/shell that launches it —
not the binary); Google Chrome for the CDP section; `pip install pillow` for the
find_image check.

The script verifies the **effect** of each action (after clicking "5" twice, OCR
must read "55"), never just "the tool returned success".

## Coverage map (macOS)

Status as of the last run. ✅ verified end-to-end · ⚠️ partial · ❌ not yet tested.

| Area | Tools | Status | Notes |
|------|-------|--------|-------|
| Displays | `get_displays` | ✅ | |
| Screenshot (visual path) | `take_screenshot` screen/window +ocr | ✅ | full-screen OCR ~1.1–1.5s; window ~250ms |
| Lifecycle | `launch_app`, `list_apps`, `list_windows`, `focus_window`, `quit_app` | ✅ | locale-robust (English / bundle id / localized) |
| AX read | `take_ax_snapshot` | ✅ | warm ~120ms; cold first-call once hit 3.2s (unexplained) |
| Text path | `find_text`, `element_at_point` | ✅ | AX-first, OCR fallback; returns screen coords |
| Coordinate input | `click`, `press_key` | ✅ | effect-verified via OCR (55 / 73) |
| Pointer | `move_mouse`, `scroll`, `drag` | ⚠️ | calls succeed; **effect not verified** |
| **AX dispatch** | `ax_click`, `ax_set_value`, `ax_select` | ❌ | **the preferred macOS path — UNVERIFIED.** Calculator's keypad is AX-opaque (`generic`), no named buttons. Needs an app with real AX buttons (System Settings / Notes / Mail). |
| `type_text` | | ⚠️ | runs, but text didn't land in TextEdit (focus); **no landing check**; 13.4ms/char; byte-count-as-char bug |
| find_image | `find_image`, `load_image` | ✅ | crop self-match score 1.0 |
| CDP | full `cdp_*` flow | ✅ | launch/navigate/snapshot/find/evaluate/disconnect |
| Background | `start/stop_hover_tracking`, `start/stop_recording` | ✅ | recording writes real JPEG frames |
| AppDebugKit | `app_connect`, `app_*` | ❌ | needs a protocol-enabled app |
| `probe_app` | | ⚠️ | returns fast; output not asserted |

## Scenario backlog (prioritized for responsiveness — "跟手")

**P0 — core path unverified or latency-critical**
- AX dispatch end-to-end (`ax_click`/`ax_set_value`/`ax_select`) on an app with
  real named AX buttons. This is the documented *preferred* macOS path and is
  currently unverified.
- `type_text`: (a) verify text actually lands (focus a real text field, OCR it);
  (b) latency — 13.4ms/char means a 500-char paste ≈ 6.7s; (c) char count is
  reported as UTF-8 **bytes** not chars ("你好世界MCP" → "15", should be 7).
- `type_text` with a **Chinese IME active** — the flagged IME-safe feature.
  Latin must land as latin (not pinyin), CJK must land correctly.
- Coordinate round-trip on Retina: `find_image` → `click` returned screen coords
  → verify the hit. The full visual loop a multimodal model relies on.

**P1 — correctness across environments**
- Multi-window app (`find_windows_by_app` main-first sort — the WeChat gotcha).
- Second display: `find_text`/`click` coordinate correctness off the main display.
- `find_text` OCR-fallback on an AX-opaque / custom-drawn app (Electron).
- Error/permission paths: missing permission → *clean error*, not silent no-op;
  app-not-found; out-of-bounds coords.
- Native tools under concurrency (only CDP non-serialization is proven so far).

**P2 — stability / depth**
- `screen_recorder` over a longer window; frame timing/integrity.
- `hover_tracker` event stream (watch for float-equality jitter producing noise).
- `app_connect` (AppDebugKit) against a protocol-enabled app.
- Investigate the cold `take_ax_snapshot` 3.2s first-call latency.

## Failure-mode log (update every session)

### Harness pitfalls (don't re-step on these)
- **`--no-default-features` overwrites the debug/release binary.** A later run
  then reports `Unknown tool: cdp_launch`. Rebuild with default features before
  driving, or check the binary first via `tools/list`.
- **`cdp_disconnect` does NOT close Chrome.** A leftover managed Chrome holds the
  shared profile (`~/.native-devtools-mcp/chrome-profile`), so the next
  `cdp_launch` times out (~45s). Clean between runs:
  `pkill -f 'native-devtools-mcp/chrome-profile'`.
  *(Product candidate: cdp_launch could detect/adopt an existing managed Chrome.)*
- **Trust the response shape, not your assumption.** Real false negatives seen:
  `find_image` returns `{"matches":[...]}` not a bare list; `find_text` returns
  JSON `"x":.. "y":..` not `(x, y)`; a **float** JSON-RPC id breaks id matching.
- **Separate permission failures from code failures.** A black screenshot / empty
  OCR means Screen Recording isn't granted to the *host process* — not a bug.
  Probe permission first (screen image bytes + OCR char count).

### Testing methodology (how to test this project well)
- **Verify effect, not "success".** Tools report sent-events, not landed-results.
  "Typed N characters" / "Clicked at (x,y)" ≠ it worked — confirm via OCR/AX read.
- **App names are localized.** On a zh-CN system Calculator is "计算器". Resolve the
  display name via `list_apps` (match `bundle_id`), or pass the bundle id.
- **Measure warm AND cold; bisect before optimizing.** `focus_window`'s ~1s was
  NOT where it looked (`list_apps` is 3ms) — it was NSWorkspace activation IPC.
  An early-return experiment located it in one build instead of guessing.
- **Fix chokepoints structurally, then grep the parallel paths.** The locale fix
  first landed only in `find_windows_by_app`; `quit_app`/`is_app_running`/
  `activate_app` each had their own copy and stayed broken until unified behind
  one predicate. After any such fix, grep for siblings doing the same thing.

### Product issues found here (status)
- ✅ fixed: app_name only matched localized name (broke English/bundle-id targeting).
- ✅ fixed: `focus_window` re-activated even when already frontmost (~1s → ~50ms).
- ✅ fixed: CDP lock held across page RPCs; blocking platform calls on the executor.
- ⬜ open: `type_text` reports byte count as char count.
- ⬜ open: `type_text` ~13ms/char; long strings slow.
- ⬜ open: `AXScrollArea` mapped to `"scrollbar"` in snapshots (should be a container).
- ⬜ open: AX dispatch path has no end-to-end verification yet.
- ⬜ open: `find_image` default downscaling can drop a *literal* crop self-match
  below a 0.9 threshold on low-detail regions — 0.9 matching is less reliable
  than it looks. Worth documenting the threshold/downscale interaction.
