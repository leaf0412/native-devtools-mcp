#!/usr/bin/env python3
"""CI test runner for native-devtools-mcp.

Runs E2E scenarios against the built binary and produces a report you can read
at a glance: per-scenario pass/fail, timing, the exact tool-call sequence that
reproduces each result, and (optionally) a screen-recorded video of the run.

Outputs (default ./e2e-report/):
  report.json   machine-readable results
  report.html   human-readable: green/red per scenario + repro call log + video
  run.mp4       screen recording of the run (if --video and ffmpeg succeed)

Exit code: 0 if every scenario passed, 1 otherwise (so CI gates on it).

Usage:
  python3 scripts/e2e/ci_runner.py [--bin PATH] [--out DIR] [--video] [--native]

  --video   record the screen with ffmpeg (GUI runs; needs Screen Recording
            permission on the host). Off by default.
  --native  also run the native-macOS suite (needs a GUI session + Accessibility
            + Screen Recording). Off by default; the headless-browser suite
            always runs and needs neither.
"""
import argparse
import html
import json
import os
import re
import subprocess
import sys
import time
from contextlib import contextmanager

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp_client import MCP  # noqa: E402


# ----------------------------------------------------------------------------- video
class ScreenRecorder:
    """Best-effort full-screen capture via ffmpeg avfoundation (macOS)."""

    def __init__(self, out_path, max_seconds=600):
        self.out = out_path
        self.max = max_seconds
        self.proc = None
        self.note = ""

    @staticmethod
    def _screen_device():
        # Parse `ffmpeg -list_devices` to find the "Capture screen 0" index.
        try:
            r = subprocess.run(["ffmpeg", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
                               capture_output=True, text=True, timeout=15)
            m = re.search(r"\[(\d+)\]\s+Capture screen 0", r.stderr)
            return m.group(1) if m else None
        except Exception:
            return None

    def start(self):
        if subprocess.run(["which", "ffmpeg"], capture_output=True).returncode != 0:
            self.note = "ffmpeg not installed"
            return
        dev = self._screen_device()
        if dev is None:
            self.note = "no avfoundation screen device (headless? permission?)"
            return
        try:
            self.proc = subprocess.Popen(
                ["ffmpeg", "-y", "-f", "avfoundation", "-framerate", "15",
                 "-i", f"{dev}:none", "-pix_fmt", "yuv420p", "-t", str(self.max), self.out],
                stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(1.0)  # let it warm up before the run starts
            if self.proc.poll() is not None:
                self.note = "ffmpeg exited immediately (Screen Recording permission?)"
                self.proc = None
        except Exception as e:
            self.note = f"ffmpeg failed to start: {e}"
            self.proc = None

    def stop(self):
        if not self.proc:
            return None
        try:
            self.proc.communicate(input=b"q", timeout=20)  # 'q' = clean finalize
        except Exception:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except Exception:
                self.proc.kill()
        return self.out if os.path.exists(self.out) and os.path.getsize(self.out) > 0 else None


# ----------------------------------------------------------------------------- report
class Reporter:
    def __init__(self, client):
        self.client = client
        self.scenarios = []

    @contextmanager
    def scenario(self, name, intent):
        start_idx = len(self.client.calllog)
        t0 = time.time()
        rec = {"name": name, "intent": intent, "checks": [], "status": "pass", "error": None}

        def check(desc, ok, detail=""):
            rec["checks"].append({"desc": desc, "ok": bool(ok), "detail": str(detail)[:300]})
            if not ok:
                rec["status"] = "fail"

        try:
            yield check
        except Exception as e:  # a thrown scenario is a failure, not a crash
            rec["status"] = "fail"
            rec["error"] = f"{type(e).__name__}: {e}"
        rec["duration_ms"] = round((time.time() - t0) * 1000)
        rec["calls"] = self.client.calllog[start_idx:]  # the reproduction trail
        self.scenarios.append(rec)
        mark = "PASS" if rec["status"] == "pass" else "FAIL"
        print(f"[{mark}] {name}  ({rec['duration_ms']}ms, {len(rec['calls'])} calls)")

    @property
    def all_passed(self):
        return all(s["status"] == "pass" for s in self.scenarios)

    def write(self, out_dir, video_path, video_note):
        os.makedirs(out_dir, exist_ok=True)
        passed = sum(s["status"] == "pass" for s in self.scenarios)
        summary = {"total": len(self.scenarios), "passed": passed,
                   "failed": len(self.scenarios) - passed,
                   "video": os.path.basename(video_path) if video_path else None,
                   "video_note": video_note}
        report = {"summary": summary, "scenarios": self.scenarios}
        with open(os.path.join(out_dir, "report.json"), "w") as f:
            json.dump(report, f, indent=2, ensure_ascii=False)
        with open(os.path.join(out_dir, "report.html"), "w") as f:
            f.write(self._html(summary))
        return summary

    def _html(self, summary):
        e = html.escape
        ok_all = summary["failed"] == 0
        head = (f"<h1>E2E report — <span style='color:{'#1a7f37' if ok_all else '#cf222e'}'>"
                f"{summary['passed']}/{summary['total']} passed</span></h1>")
        vid = ""
        if summary["video"]:
            vid = f"<video controls width='720' src='{e(summary['video'])}'></video>"
        elif summary["video_note"]:
            vid = f"<p><em>video unavailable: {e(summary['video_note'])}</em></p>"
        blocks = []
        for s in self.scenarios:
            color = "#1a7f37" if s["status"] == "pass" else "#cf222e"
            checks = "".join(
                f"<li>{'✅' if c['ok'] else '❌'} {e(c['desc'])}"
                + (f" — <code>{e(c['detail'])}</code>" if c["detail"] else "") + "</li>"
                for c in s["checks"])
            err = f"<p style='color:#cf222e'>error: <code>{e(s['error'])}</code></p>" if s["error"] else ""
            rows = "".join(
                f"<tr class='{'ok' if c['ok'] else 'bad'}'><td>{c['seq']}</td>"
                f"<td><code>{e(c['tool'])}</code></td>"
                f"<td><code>{e(json.dumps(c['args'], ensure_ascii=False))}</code></td>"
                f"<td>{c['ms']}ms</td><td>{e(c['result'])}</td></tr>"
                for c in s["calls"])
            blocks.append(
                f"<details {'open' if s['status'] == 'fail' else ''}>"
                f"<summary style='color:{color};font-weight:600'>"
                f"{'✅' if s['status'] == 'pass' else '❌'} {e(s['name'])} "
                f"<span style='color:#666;font-weight:400'>({s['duration_ms']}ms)</span></summary>"
                f"<p style='color:#666'>{e(s['intent'])}</p>{err}<ul>{checks}</ul>"
                f"<p><b>Reproduction</b> — exact tool-call sequence:</p>"
                f"<table><tr><th>#</th><th>tool</th><th>args</th><th>ms</th><th>result</th></tr>"
                f"{rows}</table></details>")
        css = ("body{font:14px -apple-system,system-ui,sans-serif;max-width:1000px;margin:2rem auto;padding:0 1rem}"
               "table{border-collapse:collapse;width:100%;margin:.5rem 0}"
               "td,th{border:1px solid #ddd;padding:4px 8px;text-align:left;vertical-align:top}"
               "code{font-size:12px;word-break:break-all}tr.bad{background:#ffebe9}"
               "details{border:1px solid #ddd;border-radius:6px;padding:.6rem;margin:.5rem 0}"
               "summary{cursor:pointer}video{margin:1rem 0;border:1px solid #ddd;border-radius:6px}")
        return f"<!doctype html><meta charset=utf-8><style>{css}</style>{head}{vid}{''.join(blocks)}"


# ----------------------------------------------------------------------------- suites
def suite_browser_headless(rep, m):
    """CI-portable: headless Chrome, no GUI session / TCC needed."""
    PORT = 9521
    with rep.scenario("browser: headless launch + navigate + evaluate",
                      "Drive a headless Chrome end-to-end via CDP (no display).") as check:
        r = m.call("cdp_launch", {"port": PORT, "headless": True, "ephemeral": True,
                                  "url": "https://example.com"}, timeout=45)
        check("cdp_launch headless+ephemeral succeeds", r["ok"], r["text"][:80])
        r = m.call("cdp_navigate", {"type": "url", "url": "https://example.com"})
        check("navigate to example.com", r["ok"] and "Navigated" in r["text"], r["text"][:60])
        r = m.call("cdp_take_dom_snapshot", {"max_nodes": 30})
        check("DOM snapshot returns UIDs", "uid=d" in r["text"], r["text"][:60])
        r = m.call("cdp_find_elements", {"query": "More information"})
        check("find_elements runs", r["ok"], r["text"][:60])
        r = m.call("cdp_evaluate_script", {"function": "() => document.title"})
        check("evaluate document.title == 'Example Domain'",
              "Example Domain" in r["text"], r["text"])
        r = m.call("cdp_disconnect", {})
        check("disconnect (kills headless Chrome, removes temp profile)", r["ok"], r["text"][:50])


def suite_native_macos(rep, m):
    """Needs a GUI session + Accessibility + Screen Recording."""
    with rep.scenario("native: screen capture + OCR (permission probe)",
                      "Screenshot the screen and OCR it; proves Screen Recording is granted.") as check:
        s = m.call("take_screenshot", {"mode": "screen", "include_ocr": True})
        check("screenshot not black (image > 5KB)", s["img"] > 5000, f"img={s['img']}b")
        check("OCR returns text (Screen Recording granted)", len(s["text"]) > 50, f"{len(s['text'])} chars")

    with rep.scenario("native: launch + locale-robust targeting + input effect",
                      "Open Calculator, target it by English name, click '5' twice, OCR reads '55'.") as check:
        m.call("launch_app", {"app_name": "Calculator"})
        time.sleep(1.3)
        apps = json.loads(m.call("list_apps", {})["text"])
        calc = next((a for a in apps if a.get("bundle_id") == "com.apple.calculator"), None)
        check("Calculator launched", calc is not None, calc["name"] if calc else "not found")
        app = calc["name"] if calc else "Calculator"
        ft = m.call("find_text", {"text": "5", "app_name": "Calculator"})  # English name on purpose
        check("find_text resolves via English name (locale-robust)", ft["ok"] and '"x"' in ft["text"], ft["text"][:50])
        try:
            arr = json.loads(ft["text"])
            x, y = arr[0]["x"], arr[0]["y"]
        except Exception:
            x = y = None
        if x is not None:
            m.call("press_key", {"key": "Escape"})
            m.call("click", {"x": x, "y": y})
            m.call("click", {"x": x, "y": y})
            time.sleep(0.4)
            v = m.call("take_screenshot", {"mode": "window", "app_name": app, "include_ocr": True})
            check("after two clicks on '5', OCR reads '55'", "55" in v["text"], v["text"][:40])

    with rep.scenario("native: AX dispatch (ax_set_value lands)",
                      "Set a value into a real text field via AX and read it back.") as check:
        m.call("launch_app", {"app_name": "System Settings"})
        time.sleep(2.5)
        apps = json.loads(m.call("list_apps", {})["text"])
        ss = next((a for a in apps if "systempreferences" in (a.get("bundle_id") or "")), None)
        if ss:
            m.call("focus_window", {"app_name": ss["name"]})
            time.sleep(0.6)
            snap = m.call("take_ax_snapshot", {"app_name": ss["name"]})["text"]
            uid = next((re.search(r"uid=(\S+)", l).group(1) for l in snap.splitlines()
                        if re.search(r"uid=\S+\s+(textbox|textfield|searchfield)\b", l)), None)
            check("found a text field in System Settings", uid is not None, uid or "none")
            if uid:
                m.call("ax_set_value", {"uid": uid, "text": "bluetooth"})
                time.sleep(0.5)
                snap2 = m.call("take_ax_snapshot", {"app_name": ss["name"]})["text"]
                check("ax_set_value value landed", "bluetooth" in snap2, "readback via fresh snapshot")
        else:
            check("System Settings available", False, "not resolved")


# ----------------------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/native-devtools-mcp")
    ap.add_argument("--out", default="e2e-report")
    ap.add_argument("--video", action="store_true")
    ap.add_argument("--native", action="store_true")
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    rec = ScreenRecorder(os.path.join(args.out, "run.mp4")) if args.video else None
    if rec:
        rec.start()

    m = MCP(args.bin)
    if not m.initialize():
        print("FATAL: initialize failed — is the binary built?")
        sys.exit(2)
    rep = Reporter(m)

    suite_browser_headless(rep, m)
    if args.native:
        suite_native_macos(rep, m)

    m.close()
    video_path = rec.stop() if rec else None
    summary = rep.write(args.out, video_path, rec.note if rec else "not requested")

    print(f"\n{summary['passed']}/{summary['total']} scenarios passed"
          + (f" | video: {summary['video']}" if summary["video"] else
             f" | video: {summary['video_note']}"))
    print(f"report: {os.path.join(args.out, 'report.html')}")
    sys.exit(0 if rep.all_passed else 1)


if __name__ == "__main__":
    main()
