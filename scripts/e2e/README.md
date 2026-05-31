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

**For CI**, use `ci_runner.py` instead — it produces a pass/fail report
(`report.html`/`report.json`), a screen-recorded video (`run.mp4`), and the
exact reproduction call-log per scenario, and exits non-zero on failure. See
**[CI.md](./CI.md)** for the GitLab setup and the (critical) self-hosted-runner
requirements. `macos_smoke.py` here is the interactive dev smoke; `ci_runner.py`
is the gated CI entrypoint.

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
| **AX dispatch** | `ax_click`, `ax_set_value`, `ax_select` | ✅ | `ax_click`/`ax_select` dispatch (System Settings button/row); `ax_set_value` **lands** on AXValue-honoring fields (System Settings search → "bluetooth", confirmed via readback). ⚠️ a multi-line `NSTextView` (TextEdit doc) ignores `AXValue` set yet still returns ok — "ok" = AX call succeeded, not effect observed. |
| `type_text` | | ✅ | char count fixed (was UTF-8 bytes). Lands when a text field is focused. ~13ms/char is inherent CGEvent cost (sleep-independent; batching breaks correctness) — **for bulk text into a field use `ax_set_value`** (sets the value at once). IME-active typing still untested. |
| find_image | `find_image`, `load_image` | ✅ | crop self-match score 1.0 |
| CDP | full `cdp_*` flow | ✅ | launch/navigate/snapshot/find/evaluate/disconnect |
| Background | `start/stop_hover_tracking`, `start/stop_recording` | ✅ | recording writes real JPEG frames |
| AppDebugKit | `app_connect`, `app_*` | ❌ | needs a protocol-enabled app |
| `probe_app` | | ⚠️ | returns fast; output not asserted |

## Scenario backlog (prioritized for responsiveness — "跟手")

**P0 — core path unverified or latency-critical**
- `type_text` with a **Chinese IME active** — the flagged IME-safe feature.
  Latin must land as latin (not pinyin), CJK must land correctly. (Done: char
  count, landing, batching/latency investigation, ax_set_value bulk path.)
- Coordinate round-trip on Retina: `find_image` → `click` returned screen coords
  → verify the hit. The full visual loop a multimodal model relies on.
- `ax_set_value` should report when the value did NOT take (e.g. NSTextView): an
  ok that means "AX call succeeded" but "effect didn't happen" is a silent trap.

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
- ✅ fixed: `type_text` reported byte count as char count.
- ℹ️ inherent: `type_text` ~13ms/char is CGEvent post cost, NOT the sleep
  (verified: latency identical at 5/2/1/0ms delay). Batching chars into one
  event truncates to a short prefix, so it's not a safe speedup. Use
  `ax_set_value` for bulk text into a field.
- ⬜ open: `ax_set_value` returns ok even when the control ignores `AXValue`
  (multi-line NSTextView) — should surface "value did not take".
- ⬜ open: `AXScrollArea` mapped to `"scrollbar"` in snapshots (should be a container).
- ⬜ open: `find_image` default downscaling can drop a *literal* crop self-match
  below a 0.9 threshold on low-detail regions — 0.9 matching is less reliable
  than it looks. Worth documenting the threshold/downscale interaction.
