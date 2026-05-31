#!/usr/bin/env python3
"""macOS end-to-end smoke test for native-devtools-mcp.

Drives the built binary over JSON-RPC against the *real* desktop. Verifies the
EFFECT of each action (e.g. after clicking '5' twice, OCR must read "55"), not
just that the tool returned success — a tool can report "Typed N characters"
while nothing landed.

Prereqs:
  - macOS GUI session (not headless/SSH).
  - Accessibility + Screen Recording granted to the HOST process that launches
    this (Terminal / the shell). Without them: black screenshots, no-op clicks.
  - cargo build --release   (must be DEFAULT features — a --no-default-features
    binary has no cdp_* tools and the CDP section will fail).
  - Google Chrome installed for the CDP section.

Run:  python3 scripts/e2e/macos_smoke.py
"""
import base64
import io
import json
import re
import sys
import time

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from mcp_client import MCP  # noqa: E402

BIN = "target/release/native-devtools-mcp"
results = []
latencies = []


def show(label, r, verify=None):
    ok = r["ok"] and (verify is None or verify(r))
    tag = "OK " if ok else "ERR"
    info = (r["text"][:68].replace("\n", " / ") if r["text"] else r["err"])
    img = f" img={r['img'] // 1000}kb" if r["img"] else ""
    print(f"  [{tag}] {label:30} {r['ms']:6.0f}ms{img}  {info}")
    results.append((label, ok))
    latencies.append((label, r["ms"]))
    return r


def coords_json(text):
    try:
        arr = json.loads(text)
        return (arr[0]["x"], arr[0]["y"]) if arr else None
    except Exception:
        return None


def main():
    m = MCP(BIN)
    if not m.initialize():
        print("FATAL: initialize failed (is the binary built?)")
        sys.exit(1)

    print("=== A. environment / permission probe ===")
    show("get_displays", m.call("get_displays", {}))
    shot = m.call("take_screenshot", {"mode": "screen", "include_ocr": True})
    # Permission proxy: a black screen compresses tiny and OCR finds nothing.
    show("take_screenshot(screen+ocr)", shot, verify=lambda r: r["img"] > 5000 and len(r["text"]) > 50)
    if shot["img"] <= 5000 or len(shot["text"]) <= 50:
        print("      !! screen looks black/empty -> Screen Recording permission likely MISSING")

    print("=== B. lifecycle + locale-robust name resolution ===")
    show("launch_app Calculator", m.call("launch_app", {"app_name": "Calculator"}))
    time.sleep(1.3)
    apps = json.loads(m.call("list_apps", {})["text"])
    calc = next((a for a in apps if a.get("bundle_id") == "com.apple.calculator"), None)
    if not calc:
        print("FATAL: Calculator not running after launch"); m.close(); sys.exit(1)
    app = calc["name"]
    print(f"      resolved Calculator display name = {app!r} (pid {calc['pid']})")
    # Locale-robustness: English name + bundle id must resolve too (regression guard).
    for nm in ("Calculator", "com.apple.calculator", app):
        r = m.call("find_text", {"text": "5", "app_name": nm})
        show(f"find_text via {nm!r}", r, verify=lambda r: coords_json(r["text"]) is not None)
    show("focus_window", m.call("focus_window", {"app_name": app}))

    print("=== C. visual path (multimodal models): window screenshot + OCR ===")
    show("screenshot(window)+ocr", m.call("take_screenshot", {"mode": "window", "app_name": app, "include_ocr": True}),
         verify=lambda r: r["img"] > 3000)

    print("=== D. coordinate input -> verify EFFECT ===")
    show("take_ax_snapshot", m.call("take_ax_snapshot", {"app_name": app}), verify=lambda r: "uid=" in r["text"])
    m.call("press_key", {"key": "Escape"})
    ft = m.call("find_text", {"text": "5", "app_name": app})
    show("find_text '5'", ft, verify=lambda r: coords_json(r["text"]) is not None)
    c = coords_json(ft["text"])
    if c:
        show("click '5' x2", m.call("click", {"x": c[0], "y": c[1]}))
        m.call("click", {"x": c[0], "y": c[1]})
        time.sleep(0.4)
        v = m.call("take_screenshot", {"mode": "window", "app_name": app, "include_ocr": True})
        ok = "55" in v["text"]
        print(f"      -> after two clicks on '5', OCR reads '55': {ok}")
        results.append(("EFFECT: click -> display 55", ok))

    print("=== E. keyboard path -> verify EFFECT ===")
    m.call("press_key", {"key": "Escape"})
    m.call("press_key", {"key": "7"}); m.call("press_key", {"key": "3"})
    time.sleep(0.4)
    k = m.call("take_screenshot", {"mode": "window", "app_name": app, "include_ocr": True})
    ok = "73" in k["text"]
    print(f"      -> after press_key 7,3, OCR reads '73': {ok}")
    results.append(("EFFECT: press_key -> display 73", ok))

    print("=== F. find_image (real crop self-match via PIL) ===")
    try:
        from PIL import Image
        full = shot["img_b64"]
        img = Image.open(io.BytesIO(base64.b64decode(full))).convert("RGB")
        w, h = img.size
        cx, cy = w // 2, h // 2
        crop = img.crop((cx, cy, cx + 180, cy + 100))
        buf = io.BytesIO(); crop.save(buf, format="PNG")
        # threshold 0.6, not 0.9: the default downscaling can drop a literal
        # crop self-match below 0.9 on low-detail regions (see README backlog).
        fi = m.call("find_image", {"screenshot_image_base64": full,
                                   "template_image_base64": base64.b64encode(buf.getvalue()).decode(),
                                   "threshold": 0.6, "max_results": 3})
        # result shape is {"matches":[{score,bbox},...]} — NOT a bare list.
        def fi_ok(r):
            try:
                return len(json.loads(r["text"]).get("matches", [])) >= 1
            except Exception:
                return False
        show("find_image crop self-match", fi, verify=fi_ok)
    except ImportError:
        print("  [SKIP] find_image (PIL not installed: pip install pillow)")

    print("=== G. CDP full flow ===")
    # NOTE: cdp_disconnect does NOT close Chrome. A leftover managed Chrome on the
    # shared profile makes the next cdp_launch time out. Kill it between runs:
    #   pkill -f 'native-devtools-mcp/chrome-profile'
    show("cdp_launch", m.call("cdp_launch", {"port": 9377}, timeout=45))
    show("cdp_navigate", m.call("cdp_navigate", {"type": "url", "url": "https://example.com"}))
    show("cdp_take_dom_snapshot", m.call("cdp_take_dom_snapshot", {"max_nodes": 40}), verify=lambda r: "uid=d" in r["text"])
    show("cdp_evaluate title", m.call("cdp_evaluate_script", {"function": "() => document.title"}),
         verify=lambda r: "Example Domain" in r["text"])
    show("cdp_disconnect", m.call("cdp_disconnect", {}))

    print("=== H. background tools ===")
    show("start_hover_tracking", m.call("start_hover_tracking", {"app_name": app, "max_duration_ms": 1500}))
    time.sleep(0.4)
    show("stop_hover_tracking", m.call("stop_hover_tracking", {}))
    show("start_recording", m.call("start_recording", {"output_dir": "/tmp/ndt_e2e_rec", "fps": 4, "max_duration_ms": 1200}))
    time.sleep(0.8)
    show("stop_recording", m.call("stop_recording", {}), verify=lambda r: "frame_" in r["text"] or "path" in r["text"])

    m.close()
    print("\n=== SUMMARY ===")
    for label, ok in results:
        if not ok:
            print(f"  FAILED: {label}")
    print(f"{sum(1 for _, ok in results if ok)}/{len(results)} checks passed")
    print("\n--- slowest calls (responsiveness) ---")
    for label, ms in sorted(latencies, key=lambda x: -x[1])[:6]:
        print(f"  {ms:7.0f}ms  {label}")


if __name__ == "__main__":
    main()
