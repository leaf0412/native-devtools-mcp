# Running the E2E suite from your CI (GitLab)

This tool is installed **on your runner machine** and *called by* your pipeline.
There is no daemon and no network service to run: a GitLab job on that machine
invokes `ci_runner.py`, which drives the built binary and writes
`e2e-report/{report.html,report.json,run.mp4}`, exiting non-zero on any failure
so the job gates on it.

## Install on the runner (once)

On the runner machine, logged into its GUI session:

```bash
bash scripts/e2e/setup-runner.sh
```

It builds the binary, checks dependencies (Chrome / python3 / ffmpeg), and runs
the **preflight** — so you learn immediately whether permissions and the GUI
session are right, instead of when CI produces black screenshots. Verify any
time with:

```bash
python3 scripts/e2e/ci_runner.py --check --native --video   # exits 0 only if the host is ready
```

## In your pipeline job

```bash
cargo build --release
python3 scripts/e2e/ci_runner.py --out e2e-report                    # headless browser only
python3 scripts/e2e/ci_runner.py --out e2e-report --native --video   # + native macOS + video
```

Publish `e2e-report/` as a job artifact and open `report.html` from the
artifacts browser. The first scenario in every run is the environment preflight;
if it fails, the native scenarios are skipped with a clear pointer here (no wall
of black-frame failures).

## The two tiers, by what they need

| Suite | Runner requirement | Permissions |
|-------|--------------------|-------------|
| **browser (headless)** | any macOS/Windows runner with Chrome + python3 | none |
| **native / Electron** | self-hosted runner **in a logged-in GUI session** | Accessibility + Screen Recording |

The headless browser suite is portable and should be your always-on gate. The
native suite is where the real setup work is — and **no CI config can replace
that setup**. Code controls behaviour; the OS controls whether your process is
even allowed to see the screen or move the mouse.

## Setting up the native macOS runner (the part that actually matters)

The recurring failure is a runner that "works" but produces **black
screenshots** and **no-op clicks**. That always comes down to one of two things:
the runner isn't in a GUI session, or it lacks TCC permissions. Both must be right.

### 1. Run the runner inside a logged-in GUI (Aqua) session

A macOS process can only touch the WindowServer (screenshots, window focus,
rendering Electron) when it runs in a logged-in user's GUI session.

- ✅ Enable **auto-login** for a dedicated user, and start `gitlab-runner` as a
  **LaunchAgent** in that user's session (`~/Library/LaunchAgents/`), or run
  `gitlab-runner run` from a Terminal in that logged-in session.
- ✅ Use the **shell executor** (not docker — Docker Desktop on macOS has no
  GUI/WindowServer).
- ❌ Do **not** install it as a system `LaunchDaemon` / via `sudo
  gitlab-runner install` running as root — that context has no GUI session.
- Keep the Mac from sleeping / locking the screen (`caffeinate`, disable screen
  lock) or the session detaches.

### 2. Grant TCC permissions to the runner's process

"Permission is granted to the host process, not the binary." Grant to whatever
launches the MCP server — usually the **shell** the runner spawns jobs with
(e.g. `/bin/zsh`, or the terminal app, or the `gitlab-runner` binary itself):

- System Settings → Privacy & Security → **Accessibility** → add it.
- System Settings → Privacy & Security → **Screen Recording** → add it.

These persist across reboots once granted. On GitHub/GitLab **hosted** macOS
runners you generally can't grant Accessibility non-interactively, which is why
native control needs a **self-hosted** runner.

### 3. Tools on the runner

- Google Chrome (standard install location).
- `python3` (3.8+) and `pip install pillow` (only for the find_image check in
  `macos_smoke.py`; `ci_runner.py` itself needs no extra packages).
- `ffmpeg` (for `--video`): `brew install ffmpeg`.

## Verifying the runner before trusting CI

Run the smoke once on the runner host, logged into its GUI session:

```bash
python3 scripts/e2e/macos_smoke.py
```

If screenshots come back as `img=0kb` / OCR is empty, **stop** — that's the
permission/session problem above, not a code bug. Fix the runner first.

## Reading the report

- `report.html` — green/red per scenario; expand a scenario for its checks and
  the **reproduction**: the exact ordered `tool(args) -> result` calls.
- `run.mp4` — screen recording of the run (native/Electron; headless has no
  on-screen video, and the report notes why if it's absent).
- `report.json` — same data, machine-readable, for dashboards.
