#!/usr/bin/env bash
# Set up a macOS machine (your GitLab runner host) to run the E2E suite.
# Builds the binary, checks dependencies, and runs the environment preflight so
# you find out *now* whether permissions/GUI session are right — not when CI
# produces black screenshots.
#
# Run this ON the runner machine, logged into its GUI session:
#   bash scripts/e2e/setup-runner.sh
set -euo pipefail
cd "$(dirname "$0")/../.."   # repo root

say() { printf '\033[1m%s\033[0m\n' "$*"; }
warn() { printf '\033[33m%s\033[0m\n' "$*"; }

say "1/4  Rust toolchain"
if ! command -v cargo >/dev/null; then
  warn "  cargo not found — install Rust (>=1.91): https://rustup.rs"; exit 1
fi
cargo --version

say "2/4  Build release binary"
cargo build --release
BIN="target/release/native-devtools-mcp"
say "  built: $BIN"

say "3/4  Dependencies"
[ -x "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" ] \
  && echo "  ✅ Google Chrome" || warn "  ⚠️  Google Chrome not at the standard path (browser suite needs it)"
command -v python3 >/dev/null && echo "  ✅ python3 ($(python3 --version 2>&1))" \
  || warn "  ⚠️  python3 missing"
command -v ffmpeg >/dev/null && echo "  ✅ ffmpeg" \
  || warn "  ⚠️  ffmpeg missing (video needs it: brew install ffmpeg)"

say "4/4  Environment preflight (permissions + GUI session)"
if python3 scripts/e2e/ci_runner.py --check --native --video --out /tmp/ndt-setup-check; then
  say "READY. Your job can now run:"
  echo "    python3 scripts/e2e/ci_runner.py --native --video"
else
  warn "NOT READY — the preflight failed. Most likely:"
  echo "  • Screen Recording / Accessibility not granted to this shell:"
  echo "      System Settings → Privacy & Security → Screen Recording / Accessibility → add your terminal/shell"
  echo "  • The GitLab runner is NOT running in a logged-in GUI session"
  echo "      (it must be a shell executor in the user's Aqua session, not a root launchd daemon)."
  echo "  See scripts/e2e/CI.md."
  exit 1
fi
