"""Minimal MCP stdio client for driving native-devtools-mcp in E2E smoke tests.

Not a unit-test dependency — this talks to a *built binary* over JSON-RPC and
exercises the real OS (windows, input, screenshots, Chrome). See README.md.
"""
import json
import queue
import subprocess
import threading
import time


class MCP:
    def __init__(self, binary, env=None):
        self.proc = subprocess.Popen(
            [binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, bufsize=1, text=True, env=env,
        )
        self.q = queue.Queue()
        threading.Thread(target=self._reader, daemon=True).start()
        self._id = 0

    def _reader(self):
        for line in self.proc.stdout:
            line = line.strip()
            if line:
                try:
                    self.q.put(json.loads(line))
                except json.JSONDecodeError:
                    pass  # stderr/log noise on stdout is ignored

    def _send(self, obj):
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def _wait(self, rid, timeout):
        start = time.time()
        pending = []
        while time.time() - start < timeout:
            try:
                r = self.q.get(timeout=timeout)
            except queue.Empty:
                break
            if r.get("id") == rid:
                for p in pending:
                    self.q.put(p)
                return r, time.time() - start
            pending.append(r)
        for p in pending:
            self.q.put(p)
        return None, time.time() - start

    def initialize(self):
        self._id += 1
        rid = self._id
        self._send({"jsonrpc": "2.0", "id": rid, "method": "initialize",
                    "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                               "clientInfo": {"name": "e2e", "version": "0"}}})
        r, _ = self._wait(rid, 10)
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        time.sleep(0.3)
        return r is not None

    def list_tools(self):
        self._id += 1
        rid = self._id
        self._send({"jsonrpc": "2.0", "id": rid, "method": "tools/list", "params": {}})
        r, _ = self._wait(rid, 10)
        return r["result"]["tools"] if r and "result" in r else []

    def call(self, name, args, timeout=40):
        # NOTE: request ids must be ints — a float id silently breaks matching.
        self._id += 1
        rid = self._id
        self._send({"jsonrpc": "2.0", "id": rid, "method": "tools/call",
                    "params": {"name": name, "arguments": args}})
        r, dt = self._wait(rid, timeout)
        if r is None:
            return {"ok": False, "err": "timeout", "text": "", "ms": dt * 1000, "img": 0, "img_b64": ""}
        # A JSON-RPC error has no "result" — do NOT treat missing result as success.
        if "error" in r:
            return {"ok": False, "err": r["error"].get("message", ""), "text": "",
                    "ms": dt * 1000, "img": 0, "img_b64": ""}
        res = r.get("result", {})
        text = "".join(c.get("text", "") for c in res.get("content", []) if c.get("type") == "text")
        imgs = [c.get("data", "") for c in res.get("content", []) if c.get("type") == "image"]
        return {"ok": not res.get("isError", False), "err": "", "text": text,
                "ms": dt * 1000, "img": sum(len(d) for d in imgs), "img_b64": imgs[0] if imgs else ""}

    def close(self):
        try:
            self.proc.terminate()
        except Exception:
            pass
