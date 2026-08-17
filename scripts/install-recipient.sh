#!/bin/bash
set -euo pipefail

# One-shot installer for the BARE binary hand-off (macOS).
# Ship this next to the `native-devtools-mcp` binary. The recipient runs:
#   ./install-recipient.sh
# It clears the Gatekeeper quarantine flag (self-built binaries are not
# notarized), makes the binary executable, then launches the built-in setup
# wizard (permissions + MCP client config).

GREEN='\033[0;32m'
RED='\033[0;31m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

# Resolve the binary sitting next to this script.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$SCRIPT_DIR/native-devtools-mcp"

echo ""
echo -e "${BOLD}native-devtools-mcp — install (bare binary)${NC}"
echo ""

if [[ "$(uname)" != "Darwin" ]]; then
    echo -e "  ${RED}✗${NC} This script is for macOS. On Windows just run the binary's"
    echo -e "    ${DIM}setup${NC} subcommand directly — no quarantine step is needed."
    exit 1
fi

if [[ ! -f "$BIN" ]]; then
    echo -e "  ${RED}✗${NC} Binary not found next to this script:"
    echo -e "    ${DIM}${BIN}${NC}"
    echo -e "    Put install-recipient.sh in the same folder as native-devtools-mcp."
    exit 1
fi

echo -e "  ${GREEN}✓${NC} Found binary: ${DIM}${BIN}${NC}"

# Self-built binaries are unsigned/unnotarized — macOS quarantines them.
xattr -dr com.apple.quarantine "$BIN" 2>/dev/null || true
chmod +x "$BIN"
echo -e "  ${GREEN}✓${NC} Cleared quarantine flag and made it executable"
echo ""

echo -e "${BOLD}Launching the setup wizard…${NC}"
echo -e "${DIM}  (permission checks + MCP client config)${NC}"
echo ""
exec "$BIN" setup
