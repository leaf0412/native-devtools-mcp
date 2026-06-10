# Pi CLI Extension Design

## Goal

Expose every currently available `native-devtools-mcp` tool to Pi Agent as a
native Pi tool without requiring Pi to support MCP. A Pi extension owns one
long-running `native-devtools-mcp cli` process so stateful workflows such as AX
dispatch, CDP, Android, image caches, hover tracking, and screen recording keep
their state across tool calls.

The existing MCP server remains unchanged.

## Scope

- Add a long-running JSONL CLI mode at `native-devtools-mcp cli`.
- Reuse `ToolRegistry` for tool schemas and dispatch.
- Add a Pi package extension that dynamically registers each available tool.
- Preserve text and image tool results.
- Keep CLI calls sequential within one Pi session.
- Document installation and binary discovery.

The first version does not add a daemon, socket transport, automatic process
restart, binary download, or a second implementation of any platform tool.

## Binary Discovery

The Pi extension resolves the executable in this order:

1. The path in `NATIVE_DEVTOOLS_BIN`.
2. `native-devtools-mcp` found through `PATH`.

It does not inspect a repository `target/release` directory. If neither source
resolves to an executable, extension startup fails with an actionable error that
names both supported configuration methods.

## Architecture

### Shared Rust Session

Extract the MCP server's stateful tool dependencies into a reusable session
component. It owns the same shared state currently used by `MacOSDevToolsServer`:

- AppDebugKit client
- screenshot and image caches
- Android device
- hover tracker
- screen recorder
- CDP client when enabled
- AX session on macOS

Both the MCP server and JSONL CLI build `ToolContext` values from this component.
`ToolRegistry` remains the single source of truth for tool names, schemas,
availability, and implementations.

Replace the concrete MCP `Peer` field in `ToolContext` with a narrow tool-list
change notifier. The MCP adapter forwards notifications to
`notify_tool_list_changed`; the CLI adapter records no notification because its
caller explicitly refreshes the list. Tool handlers call this interface rather
than depending directly on MCP transport types.

Dynamic CLI availability is derived by querying the session state after calls,
not by depending on transport notifications.

### JSONL CLI Runner

`native-devtools-mcp cli` starts Tokio once, initializes platform support and
logging, then reads newline-delimited JSON from stdin until shutdown or EOF.
It processes one request at a time. Stdout contains JSONL responses only; logs
and diagnostics go to stderr.

Supported methods:

```json
{"id":"1","method":"list_tools","params":{}}
{"id":"2","method":"call_tool","params":{"name":"click","arguments":{"x":500,"y":300}}}
{"id":"3","method":"shutdown","params":{}}
```

Success response:

```json
{"id":"2","ok":true,"result":{"content":[]}}
```

Failure response:

```json
{"id":"2","ok":false,"error":{"code":"invalid_params","message":"missing required param: x"}}
```

Every valid request must contain a string `id`. The response echoes it exactly.
Malformed JSON has no trustworthy ID and returns `id: null` with code
`parse_error`. Unknown methods and tools return structured errors without
terminating the process. EOF and `shutdown` perform orderly cleanup.

### Pi Extension

The extension starts one CLI child process per Pi process and maintains:

- a monotonically increasing request ID
- a map of pending requests
- a JSONL stdout parser
- captured stderr diagnostics
- a serialized call queue
- the last known available tool set

At startup it calls `list_tools`, converts each MCP JSON Schema into the schema
accepted by Pi, and registers one native Pi tool per returned entry. Tool names
remain unchanged, for example `take_screenshot`, `click`, and `cdp_connect`.
Descriptions and parameter schemas come from Rust rather than being duplicated
in TypeScript.

After every tool call, including failed calls, the extension calls `list_tools`
again. Newly available names are registered immediately. Registered tools that
are no longer available are removed from Pi's active tool set while retaining
their definitions for possible later reactivation.

When changing Pi's active tool set, the extension reads the current set and only
adds or removes names owned by this extension. It preserves built-in tools and
tools registered by other extensions.

The extension handles `session_shutdown` by sending `shutdown`, waiting briefly
for normal exit, and then terminating the owned process if it has not exited.
This final termination is lifecycle cleanup, not an automatic recovery path.

## Result Conversion

MCP text content becomes Pi text content. MCP image content becomes Pi image
content with its MIME type and base64 payload preserved. Other result metadata
is retained in the Pi result `details` field for diagnostics.

Tool errors throw from the Pi tool `execute` function so Pi marks the result as
failed. The extension does not turn errors into successful text responses.

Pi may request cancellation before a queued call starts; that call is rejected
without being sent. Once a real-machine operation has been sent, cancellation
does not claim the operation was stopped because the CLI protocol has no
operation-level cancellation guarantee.

## Concurrency And Safety

All native-devtools calls in one extension instance execute sequentially. This
prevents concurrent mouse actions and protects stateful sequences such as
snapshot generation followed by AX dispatch. The CLI runner also dispatches
requests sequentially, so behavior stays deterministic even for non-Pi clients.

The extension never logs request arguments by default because they can contain
typed text or other sensitive data. Errors include method and tool names but no
full argument dump.

## Failure Behavior

- Invalid JSON: return `parse_error`; continue reading.
- Invalid request shape: return `invalid_request`; continue reading.
- Unknown method: return `method_not_found`; continue reading.
- Unknown or unavailable tool: return a structured tool error; continue reading.
- Tool failure: return the original error message and a stable error code.
- Child process exit: reject every pending and queued request; do not restart.
- Invalid child stdout: fail the extension session; do not skip the line.
- Binary missing: fail startup with configuration instructions.

There is no retry, fallback transport, silent skip, or automatic restart.

## Packaging

The repository root becomes an installable Pi package through a `pi` manifest in
its package metadata, pointing to a TypeScript extension directory. Pi can load
it from a local checkout during development or from the Git repository after
publication.

The extension has no runtime dependency on the repository layout. It only needs
the Pi-provided extension APIs and a resolvable native-devtools executable.

## Verification

Rust tests cover:

- request parsing and response ID correlation
- `list_tools` availability
- `call_tool` dispatch through `ToolRegistry`
- malformed JSON and unknown methods without process termination
- multiple requests sharing one session
- stdout JSONL purity
- orderly shutdown

Pi extension tests cover:

- executable discovery precedence
- child process request correlation
- partial-line buffering and multiple responses per chunk
- schema-driven native tool registration
- text and image result conversion
- sequential call ordering
- dynamic activation and deactivation
- pending request rejection on child exit
- shutdown cleanup

Integration verification uses tmux to start Pi with the extension, confirms the
native tools are visible, invokes at least one read-only tool, and verifies the
owned CLI process exits with Pi. Rust tests, extension tests, and the release
build must pass before implementation commits are created.
