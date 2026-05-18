# HSP — Harness Server Protocol

A standalone MCP server that bridges the Language Server Protocol into agent harnesses, exposing LSP-backed operations as typed MCP tools. HSP keeps LSP protocol details inside the server while exposing graph-oriented operators for agents: find semantic nodes, inspect nodes, expand graph edges, stage mutations, verify. It ships as a Claude Code and Codex CLI plugin with built-in routing for Python, C#, and Rust.

HSP is more than an LSP bridge. It is a full agent coordination runtime: a persistent broker daemon shares warm language servers across agents, an event bus tracks parallel work, a render memory system compresses repeated semantic output into stable aliases, and a workgroup hierarchy scopes all of this to the right social boundary.

```
Claude Code / Codex CLI / any MCP client
    ↕ MCP (stdio)
hsp mcp
    ↕ JSONL / Unix socket
hsp-broker (singleton daemon)
    ├── LSP chains (ty, basedpyright, csharp-ls, rust-analyzer)
    ├── AgentBus (event log, tickets, questions, presence)
    ├── AliasCoordinator (render memory)
    └── BabelBridge (terminal UX bus, optional)
```

## Table of Contents

- [Tools](#tools)
- [How the Model Calls the Tools](#how-the-model-calls-the-tools)
- [Plugin Install](#plugin-install)
- [CLI](#cli)
- [Architecture](#architecture)
- [Broker](#broker)
- [Agent Bus](#agent-bus)
- [Workgroup System](#workgroup-system)
- [Render Memory](#render-memory)
- [Hook System](#hook-system)
- [Build Gate](#build-gate)
- [Edit Gate](#edit-gate)
- [Language Router](#language-router)
- [LSP Chain](#lsp-chain)
- [File Watcher](#file-watcher)
- [Python Import Rewriter](#python-import-rewriter)
- [Babel Bridge](#babel-bridge)
- [Skills](#skills)
- [Configuration Reference](#configuration-reference)
- [Standalone Usage](#standalone-usage)
- [For LSP Plugin Authors](#for-lsp-plugin-authors)
- [Harness Compatibility](#harness-compatibility)
- [Feature Preservation Ledger](#feature-preservation-ledger)
- [Design Documents](#design-documents)
- [License](#license)

## Tools

25 MCP tools organized into six groups. The design principle is documented in [docs/tool-surface.md](docs/tool-surface.md): `find semantic nodes → inspect nodes → expand graph edges → stage mutations → verify`.

### Semantic Navigation

| Tool | Purpose |
|------|---------|
| `lsp_grep` | Text search plus semantic binding — groups identifier hits by symbol identity. Returns `refs N` (LSP-verified) or `hits N (unresolved)` (text-only fallback). |
| `lsp_symbols_at` | Expands all semantic symbols on a source line, including function args. |
| `lsp_symbol` | Inspects one semantic node: kind, type, hover/docs, definition, scope, signature. |
| `show_definition` | Shows where a semantic node is defined. |
| `show_declaration` | Shows where a semantic node is declared. |
| `show_type` | Shows where the type behind a semantic node is defined. |
| `show_implementation` | Shows concrete implementations for a semantic node. |
| `show_origins` | Combined: definition + declaration + type + implementation in one call. |

### Graph Operators

| Tool | Purpose |
|------|---------|
| `lsp_refs` | Expands references for a semantic node or graph index. Multi-match fan-out for overloaded symbols. |
| `lsp_calls` | Walks incoming/outgoing call graph edges via `direction=in|out|both`. BFS with configurable depth and edge budgets. |
| `lsp_types` | Walks super/sub type hierarchy edges via `direction=super|sub|both`. |
| `lsp_path` | Bounded witness path between two semantic anchors via call graph. BFS with hub pruning. |

### Structure & Verification

| Tool | Purpose |
|------|---------|
| `lsp_outline` | Compact file/workspace symbol breadcrumbs. Supports multi-file batching. |
| `lsp_diagnostics` | Errors/warnings for files. Supports comma-separated paths and glob patterns. |

### Mutations

| Tool | Purpose |
|------|---------|
| `lsp_rename` | Preview symbol rename across workspace. Stages the edit for `lsp_confirm`. |
| `lsp_move` | Preview file/symbol moves with import-updating edits. Single, symbol-to-file, or batch (`from=>to` pairs). Falls back to Python import rewriter when LSP returns 0 edits. |
| `lsp_fix` | Surface code actions (quick fixes, refactorings). Diagnostics shown as `(d0)`, `(d1)`, etc. Edit-backed actions staged for `lsp_confirm`. Supports `kind` filter. |
| `lsp_confirm` | Apply one staged candidate from a pending preview. Named stages support parallel agent flows. |

### Coordination

| Tool | Purpose |
|------|---------|
| `lsp_log` | Agent-bus surface with 17 actions: `event`, `note`, `ask`, `reply`, `chat`, `ticket`, `journal`, `question`, `edit_gate`, `recent`, `settle`, `precommit`, `postcommit`, `weather`, `presence`, `workgroup`, `status`. |
| `ticket` | Acquire or release work tickets for agent coordination. |
| `journal` | Compact workgroup journal plus open tickets/questions. |
| `ask` | Open a timed coordination question and block-wait for replies or timeout. |
| `chat` | Post a chat row, optionally replying to and closing an ask. |

### Session & Memory

| Tool | Purpose |
|------|---------|
| `lsp_session` | Inspect and manage LSP sessions. Actions: `status`, `add`, `warm`, `restart`, `stop`. |
| `lsp_memory` | Inspect and manage render-memory aliases. Actions: `status`, `legend`, `recall`, `reset`. |

## How the Model Calls the Tools

**Semantic targets, not raw protocol calls.** Tools accept graph indices, bare `Lxx`, `file:Lx`, unique basenames, or `file_path` plus `symbol`/`line`:

```
lsp_symbol(file_path="src/app.py", symbol="OmfiApp")
show_origins(file_path="src/app.py", symbol="workflow", line=476)
lsp_refs(target="[0]")           # graph index from previous lsp_grep/lsp_symbols_at
lsp_symbols_at("L78")            # bare Lxx — resolves against the last printed graph
lsp_symbols_at("HistoryUI.cs:L78")  # basename + line, no full path required
lsp_symbol("A3")                 # render-memory alias
```

**Batching.** Multiple symbols in one file, multiple files in one call:

```
lsp_diagnostics(file_path="a.py,b.py,c.py")
lsp_diagnostics(pattern="src/**/*.py")
```

**Output format.** Line-number-anchored text, no JSON envelopes. Each response is prefixed with `[server method]`:

```
[ty textDocument/hover]
<class 'OmfiApp'>
Standalone ComfyUI frontend built on AppKit.
```

Sample lists shown by `lsp_grep` (`samples L57,L694,...`) are non-exhaustive — a trailing `...` means more refs exist; unfold with `lsp_refs([N])` or raise `max_hits`.

## Plugin Install

HSP ships as one unified plugin with broker-owned routing. Built-in routes:

| Route | LSP chain | Selection signals |
|-------|-----------|-------------------|
| Python | `ty server` → `basedpyright-langserver --stdio` | `.py`, `.pyi`, `pyproject.toml`, `setup.py`, `setup.cfg` |
| C# | `csharp-ls` | `.cs`, `*.sln`, `*.csproj`, `Directory.Build.props`, `global.json` |
| Rust | `rust-analyzer` | `.rs`, `Cargo.toml`, `rust-project.json` |

Each request's target URI is forwarded to the broker, which selects a route from file extension or workspace markers. Route-specific LSP chain, method cache, warmup state, and broker session stay separate.

Force a route for workspace-level operations: `HSP_ROUTE=python`, `HSP_ROUTE=csharp`, or `HSP_ROUTE=rust`. Explicit `LSP_SERVERS` or legacy `LSP_COMMAND` overrides still work and keep single-chain mode.

Legacy split plugins: [hsp-cs](https://github.com/holo-q/hsp-cs) (C#), [hsp-py](https://github.com/holo-q/hsp-py) (Python).

## CLI

The `hsp` binary provides seven subcommands:

| Command | Purpose |
|---------|---------|
| `hsp` | Workgroup status: resolved root, workspace ID, project root, gate policy, broker status, journal weather. |
| `hsp mcp` | Start the MCP server over stdio. This is what plugin manifests invoke. |
| `hsp log <action>` | Direct access to all 17 bus actions plus the `hook` alias for event recording. |
| `hsp hook stdin <kind>` | Claude Code plugin hook adapter. Reads hook JSON from stdin, processes gates, records events. |
| `hsp run -- <command>` | Wait on the build gate, execute a command, record the result as `test.ran`. |
| `hsp watch` | Live event tail. `--global` for all workspaces, `--once` for scripts, `--exact` for single root. |
| `hsp workgroup [locations...]` | Multi-location workgroup diagnostics with `--lsp` for session details. |
| `hsp global` | Broker-global status: sessions, LSP clients, routes, devtools, babel bridge. |

Bare `hsp` never blocks on stdio — it prints the workgroup debug surface and exits.

### Entry Points

```toml
hsp              = "hsp:main"              # CLI
hsp-ty           = "hsp:mcp_main"          # Python MCP (distinct command for dedup)
hsp-csharp       = "hsp:mcp_main"          # C# MCP
hsp-redirect-hook = "hsp.redirect_hook:main" # LSP redirect hook
hsp-broker       = "hsp.broker:main"       # Standalone broker daemon
```

Per-language aliases exist because Claude Code deduplicates MCP servers by command string.

## Architecture

### Request Flow

1. Agent calls an MCP tool (e.g., `lsp_refs(target="[0]")`)
2. Tool wrapper (`_wrap_with_header`) drains agent messages, records heartbeat, prepends `[server method]` header
3. Semantic target resolution: alias lookup → graph index `[N]` → bare `Lxx` → `file:Lx` → `file_path+symbol+line`
4. Route activation: `_bind_route_runtime` swaps module-level globals to the correct language's `RouteRuntime`
5. Request dispatch:
   - **Broker path** (default): forward to persistent broker via Unix socket
   - **Direct path** (fallback): use local `LspClient` instances
6. Chain traversal: try primary server, fall back on `-32601` or empty results, cache the winner per method
7. Render: format results, assign aliases via render memory, record semantic nav context

### RouteRuntime

Each language route has its own isolated state: chain configs, LSP clients, method handler cache, warmed folders, warmup stats. `_bind_route_runtime` swaps module-level globals to point at the active route — this is how one 6000-line server module hosts multiple language chains without restructuring.

### Preview/Confirm

Mutations (rename, move, fix) stage `Candidate` objects in a `PendingBook`. The agent previews edits, then `lsp_confirm(index)` applies one candidate to disk. Named handles allow parallel previews without collision. `WorkspaceEdit` application handles both `changes` and `documentChanges` formats, supports file create/rename/delete, and is UTF-16 aware.

## Broker

The broker (`hsp-broker`) is a user-level singleton daemon that centralizes LSP server lifecycle across multiple MCP sessions. N agents working on the same workspace share one warm language server chain.

### Session Identity

Sessions are keyed by `SessionKey(root, config_hash)`. Two clients with the same key share the same session. The `config_hash` is a 12-char SHA-256 digest of language + chain configuration.

### Wire Protocol

Unix domain socket with JSONL framing (one JSON object per newline). Socket at `$XDG_RUNTIME_DIR/hsp-broker.sock` by default.

```json
{"id": "c1", "method": "lsp.request", "params": {...}}
{"id": "c1", "result": {...}}
```

### Wire Methods (40+)

**Core:** `ping`, `status`, `shutdown`

**Session:** `session.get_or_create`, `session.list`, `session.stop`, `session.stop_matching`

**LSP:** `lsp.status`, `lsp.request`, `lsp.add_workspace`, `lsp.diagnostics`, `lsp.notify_files`

**Render memory:** `render.touch`, `render.lookup`, `render.status`, `render.reset_client`, `render.reset_session`

**Agent bus (20 plus aliases):** `bus.status`, `bus.event`/`bus.append`, `bus.heartbeat`, `bus.ticket`, `bus.journal`, `bus.chat`, `bus.question`, `bus.build_gate`, `bus.edit_gate`, `bus.note`, `bus.ask`, `bus.reply`, `bus.recent`, `bus.recent_all`, `bus.recent_tree`, `bus.settle`, `bus.precommit`, `bus.postcommit`, `bus.weather`, `bus.presence`/`bus.workgroup`

### Lifecycle

- **Auto-start:** `BrokerClient.connect_or_start()` spawns the broker as a detached subprocess if missing, polls for readiness up to 10s.
- **Lazy LSP:** Language servers are NOT spawned at session creation — only on first request that needs them.
- **Idle eviction:** Sessions unused for `HSP_BROKER_IDLE_TTL_SECONDS` (default 4 hours) are stopped.
- **Graceful shutdown:** `shutdown` wire method stops all LSP, devtools, and babel bridge; SIGINT/SIGTERM trigger the shutdown event.
- **Devtools:** `LSP_DEVTOOLS=1` registers live broker, bus, registry, and LSP objects with `python-devtools` for runtime introspection.

## Agent Bus

The coordination layer for parallel agents working in the same workspace. Feels like weather, not a traffic cop: compact situational awareness injected at natural boundaries so agents adjust course without lock rituals.

```
append events → hold tickets → ask/chat → inject compact digests at bus stops
```

Full design in [docs/agent-bus.md](docs/agent-bus.md).

### Events

34 event kinds across 7 categories:

| Category | Kinds |
|----------|-------|
| Lifecycle | `agent.started`, `agent.heartbeat`, `session.start`, `session.stop`, `subagent.stop` |
| User | `prompt`, `user.prompt`, `task.intent` |
| Tool | `tool.before`, `tool.after`, `confirm.before`, `confirm.after` |
| Edit | `edit.before`, `edit.after`, `file.touched`, `symbol.touched` |
| Git | `commit.before`, `commit.after`, `commit.created`, `push.before`, `push.after` |
| Communication | `notification`, `note.posted`, `chat.message`, `bus.ask`, `bus.reply`, `bus.closed` |
| Work | `ticket.started`, `ticket.joined`, `ticket.released`, `ticket.closed`, `compact.before`, `test`, `test.ran`, `babel.event` |

Events are append-only, workspace-scoped, and persisted to `tmp/hsp-bus.jsonl`. Each carries: `event_id`, `kind`, `timestamp`, `workspace_id`, `workspace_root`, `agent_id`, `client_id`, `session_id`, `task_id`, `git_head`, `dirty_hash`, `scope` (files/symbols/aliases), `message` (max 8 KiB), `metadata`.

### Scope

`BusScope` is the filtering primitive: three optional tuple fields — `files`, `symbols`, `aliases`. An empty scope is a wildcard (workspace-wide notes hit everything). `overlaps()` checks intersection across all three dimensions.

### Tickets

Work declarations — one per agent. Starting a ticket signals "I am changing the substrate":

```
hsp.ticket("feat-workgroup-ticket-state", files="src/hsp/agent_bus.py")
hsp.ticket("")   # release
```

Ticket titles are required for start/join and must be lowercase hyphen-separated slugs prefixed with `fix`, `feat`, `docs`, `refactor`, `test`, `chore`, `perf`, `build`, `ci`, `style`, `revert`, `review`, `debug`, `ops`, or `release`; empty still means release. The broker coalesces identical titles. First holder emits `ticket.started`; later holders emit `ticket.joined`; release emits `ticket.released`; last release emits `ticket.closed`. Scope accumulates: files, symbols, and aliases merge across join calls.

### Questions

Timed coordination windows (default 3 minutes):

```
hsp.ask("Anyone touching server.py?", files="src/server.py", timeout="3m")
hsp.chat("I'm done with that section", id="Q3")
```

During the timeout, bus stops show a compact reminder. At timeout, the next stop emits a digest with replies, related events, and suggested actions. If no agents are active (no ticket holders), the question closes immediately with a no-replier notice.

### Presence

Derived from the event stream. Every event is an implicit heartbeat.

| State | Threshold | Visibility |
|-------|-----------|------------|
| `active` | < 60s since last event | Shown in weather/recent |
| `asleep` | 60s – 600s | Shown dimmed |
| `pruned` | > 600s | Hidden unless pinned |

Agents with `prompt_count >= 2` are pinned — they never collapse to pruned.

### Journal

`hsp.journal()` shows the compact shared board: open tickets, open questions, latest event rows (default 25). One-line format: `E1 08:09:10 ticket.started feat-workgroup-ticket-state`.

## Workgroup System

Hierarchical scope discovery with two orthogonal layers: **workgroups** (social coordination) and **projects** (build/check).

### Discovery

1. Walk ancestors from location looking for `workgroup.toml` or `.hsp/workgroup.toml`
2. Deepest marker = active workgroup; parents form escalation stack
3. Boundary controlled by `HSP_WORKGROUP_BOUNDARY` env var
4. Override via `HSP_WORKGROUP_ROOT` env var
5. No markers found → cwd becomes ephemeral workgroup

### workgroup.toml

```toml
[workgroup]
name  = "repo-agent"
level = "domain"       # "umbrella" for org root, "domain" for workspace dirs
color = "#33B8A6"      # truecolor hex or ANSI name

[observe]
mode = "subtree"       # exact | subtree | network
roots = ["repo-os", "repo-agent"]  # for network mode
```

### Observation Modes

- `subtree` (default): see all children under the workgroup root
- `exact` / `self`: only this root
- `network` / `roots` / `explicit`: configured explicit roots

### Project Root Detection

Uses language markers from built-in routes plus extras: `package.json`, `pnpm-workspace.yaml`, `go.mod`, `justfile`, `*.slnx`. Bounded by the active workgroup root. Build gates key on project root, not workgroup root — two projects in one workgroup don't block each other's compiles.

## Render Memory

Context-aware compression for agent output. Assigns stable short aliases to semantic identities so agents can refer to symbols across tool calls without re-resolving.

### Alias Grammar

| Family | Prefix | Example | Purpose |
|--------|--------|---------|---------|
| Symbol | A–Z (skipping F, T), AA, AB... | `A3`, `B1` | Methods, fields, classes, free functions |
| File | `F` | `F1` | File paths |
| Type | `T` | `T1` | Type aliases |

Bucket/member structure preserves topology: `A3 -> A7` is intra-container, `A3 -> J1` is cross-container.

### Epochs

Aliases are valid within an epoch. An epoch ends on: LSP session restart, workspace root change, confirmed mutation touching aliased files, explicit reset, or broker session boundary. Within an epoch, aliases are monotonic — retired aliases are never recycled.

### Compression Levels

| Level | Name | Shape | When |
|-------|------|-------|------|
| L0 | Verbose | `[3] L44 ::ComfyNodeRenderer:: method Render: void - refs 9` | Cold output |
| L1 | Chipped | `[3] A3 L44 ::ComfyNodeRenderer:: method Render: void` + legend | First alias introduction |
| L2 | Alias First | `[3] A3 Render: void - refs 9` | Warm symbol |
| L3 | Dense | `[P0] A3 -> A7 -> J1` | Path/call chains |

### Multi-Agent Coordination

`AliasCoordinator` keeps a master alias book shared across agents. Each client has its own introduction frontier — a second agent receives the same canonical alias `A3` with its own first-use legend, while a warmed agent gets the compressed handle without another legend wall.

```
legend gen=12:
  A=ComfyNodeRenderer.cs::ComfyNodeRenderer  A3=Render@L44  A7=Update@L88
  J=NodeImageStore.cs::NodeImageStore        J1=Get@L21
```

## Hook System

HSP ships 13 hook entry points covering every Claude Code lifecycle event. All hooks use the `hsp hook stdin <kind>` adapter which reads hook JSON from stdin, processes gates, and records bus events.

### Shipped Hooks

| Event | Kind | Purpose |
|-------|------|---------|
| `SessionStart` | `session.start` | Emit weather |
| `SessionEnd` | `session.end` | Record session boundary |
| `Stop` | `session.stop` | Record clean stop |
| `StopFailure` | `stop.failure` | Record abnormal termination |
| `UserPromptSubmit` | `prompt` | Emit weather, track prompt count |
| `SubagentStart` | `subagent.start` | Track child agent |
| `SubagentStop` | `subagent.stop` | Record child termination |
| `Notification` | `notification` | Record notifications |
| `PreCompact` | `compact.before` | Record context compaction |
| `PostCompact` | `compact.after` | Record compaction result |
| `PermissionRequest` | `permission.request` | Observe permission decisions |
| `PreToolUse` (Edit/Write) | `edit.before` | Edit gate check, file-scoped context injection |
| `PostToolUse` (Edit/Write) | `edit.after` | Record edit completion |
| `PreToolUse` (catch-all) | `tool.before` | Build gate for detected build commands |
| `PostToolUse` (catch-all) | `tool.after` | Record tool completion |
| `PreToolUse` (LSP) | redirect | Deny Claude's built-in LSP tool, redirect to HSP MCP tools |

### LSP Redirect Hook

A `PreToolUse` hook intercepts every call to Claude's built-in `LSP` tool and denies it with a redirect message listing all HSP MCP tools. This is how HSP replaces native LSP — by intercepting and teaching, not by disabling.

### Context Injection

Read/edit hooks query `bus.recent` for file-scoped activity and render compact agent-annotated tickets, open questions, and recent journal rows before the action continues. Controlled by `HSP_HOOK_CONTEXT` (default on).

### Controlling Hooks

- `HSP_HOOKS=0` — drain stdin and exit without recording events
- `HSP_HOOK_CONTEXT=0` — hooks record but don't inject context

## Build Gate

The one intentional stop sign. Build hooks and `hsp run` call the gate before running expensive commands.

### Gate Logic

Returns `unlocked` when:
- **`clear`** — no active ticket holders overlap the requested scope
- **`all_waiting`** — every holder of overlapping tickets is also waiting at the build gate (deadlock prevention)

Returns `locked` when:
- **`active_tickets`** — some holders are still actively editing

### Build Command Detection

HSP recognizes 23+ build tools across ecosystems: `cargo`, `npm`, `go`, `uv`, `make`, `dotnet`, `pytest`, `ruff`, `mypy`, `eslint`, `prettier`, `biome`, `bun`, `deno`, and more. Commands are classified as full-workspace or file-scoped.

### Authoritative Build Batching

When a build gate unlocks because all holders are waiting, HSP runs the command once under `tmp/hsp-build-batches/`, captures output, records the `test.ran` result, and returns a denial payload so duplicate executions are suppressed. File-lock based dedup with configurable TTL (`HSP_BUILD_BATCH_TTL`, default 30s).

```
hsp run -- cargo test
```

## Edit Gate

Optional hard policy. When `HSP_REQUIRE_TICKET_FOR_EDITS=1`, edit-before hooks check whether an active ticket exists for the workspace. If not, the edit is denied with a harness-native denial payload.

Two scopes:
- `workgroup` (default): any active ticket in the workspace unlocks edits
- `agent`: only the specific agent's own ticket unlocks (`HSP_EDIT_GATE_SCOPE=agent`)

## Language Router

Automatic language detection from file extensions and project markers.

### Built-In Routes

| Route | Extensions | Markers | Default Chain |
|-------|-----------|---------|---------------|
| Python | `.py`, `.pyi` | `pyproject.toml`, `setup.py`, `setup.cfg`, `.git` | `ty server` → `basedpyright-langserver --stdio` |
| C# | `.cs` | `*.sln`, `*.csproj`, `Directory.Build.props`, `global.json`, `.git` | `csharp-ls` |
| Rust | `.rs` | `Cargo.toml`, `rust-project.json`, `.git` | `rust-analyzer` |

Route resolution: file extension match first, then project root markers (deepest wins). Each route carries an `env` dict that overlays `os.environ` for chain-specific configuration.

Controlled by `HSP_ROUTER` (default: enabled). Disabled when explicit `LSP_SERVERS`/`LSP_COMMAND` is set.

## LSP Chain

Multiple LSP servers form a chain — primary plus fallbacks. The chain handles per-method routing with caching.

### Method Routing

1. **Fast path**: If `method_handler[method]` is cached, send directly to that chain index
2. **Cold path**: Try each server in order. On `-32601` (method not supported), try next. On empty result for `empty_fallback_methods`, try next.
3. **Cache winner**: First success is cached per method
4. **Permanent disable**: All servers returning `-32601` caches the method as `None`

### Special Handling

- `LSP_PREFER` pre-seeds the routing cache (e.g., pin call hierarchy to basedpyright)
- `LSP_REPLACE` swaps commands post-parse (e.g., basedpyright → pylance)
- `LSP_EMPTY_FALLBACK` controls which methods cascade on empty results (default: `textDocument/references`, `workspace/symbol`)
- rust-analyzer gets special retries: 8 retries for null `documentSymbol`, 6 for empty `references`

### Document Sync

`LspClient` communicates via JSON-RPC 2.0 over stdio. Documents are synced from disk before every request via `resync_open_documents()` which does an O(N) mtime sweep. `ensure_document()` sends `didOpen` on first access, then `didChange` on subsequent calls.

### Warmup

When a workspace folder is added, bulk `didOpen` fires for files matching `LSP_WARMUP_PATTERNS` (up to `LSP_WARMUP_MAX_FILES`, default 500). This prevents the cold-index failure mode where `willRenameFiles` returns 0 edits because nothing has been indexed.

## File Watcher

Bridges OS filesystem events into LSP `workspace/didChangeWatchedFiles` notifications via `watchdog`. Watches `.py`/`.pyi` files recursively with 100ms debounce. Excludes 18+ directories (`.venv`, `node_modules`, `__pycache__`, `.git`, etc.). Also pushes `textDocument/didChange` for already-opened documents.

## Python Import Rewriter

Regex-driven import rewriting for file moves, filling a gap where basedpyright only rewrites explicit re-exports during `workspace/willRenameFiles` but ignores ordinary `from X import Y` imports.

Handles two layouts: `src/` layout (`repo/src/pkg/mod.py` → `pkg.mod`) and flat layout (walks up while `__init__.py` exists). Rewrites 6 import patterns. Output is a standard LSP `WorkspaceEdit` that merges with the server's own edits. Scans up to 5000 files per workspace.

## Babel Bridge

Subscribes to the Babel paint-event daemon's Unix socket and translates 15 Babel event names into HSP bus events. Activated via `HSP_BABEL_BRIDGE=1` on the broker.

Babel events are stored with `metadata.source=babel` and normalized to workgroup concepts: `window_added` → `agent.started`, `tool_started` → `tool.before`, `daemon_shutdown` → `session.stop`, etc.

Best-effort: if Babel isn't running, HSP keeps working normally.

## Skills

Six skills ship under `plugins/hsp/skills/`:

### Language LSP Skills

| Skill | Languages | What It Teaches |
|-------|-----------|-----------------|
| `python-lsp` | Python | Use ty + basedpyright chain; symbol names first, `line=` for disambiguation |
| `csharp-lsp` | C# | Use csharp-ls; same semantic target patterns |
| `rust-lsp` | Rust | Use rust-analyzer; same semantic target patterns |

### Coordination Skills

| Skill | Purpose |
|-------|---------|
| `workgroup-coordination` | Multi-agent coordination loop: check journal, hold ticket, read context, ask/chat, release |
| `work-session` | Session start protocol: orient to workgroup, check journal, hold ticket, coordinate, release |
| `work-ticket` | Ticket lifecycle: start before edits, scope with files/symbols, release when done |

## Configuration Reference

### Broker & Daemon

| Variable | Default | Description |
|----------|---------|-------------|
| `HSP_BROKER` | `auto` | Broker mode: `auto` (share + fallback), `on` (require), `off` (direct only) |
| `HSP_BROKER_SOCKET` | `$XDG_RUNTIME_DIR/hsp-broker.sock` | Override broker socket path |
| `HSP_BROKER_LOG` | `~/.local/state/hsp/broker.log` | Override broker log path |
| `HSP_BROKER_IDLE_TTL_SECONDS` | `14400` (4h) | Idle session eviction. `0` disables. |

### Language Router

| Variable | Default | Description |
|----------|---------|-------------|
| `HSP_ROUTER` | `on` | Enable/disable auto-routing. Disabled when explicit `LSP_SERVERS`/`LSP_COMMAND` is set. |
| `HSP_ROUTE` | (auto) | Force a specific route: `python`, `csharp`, `rust` |

### LSP Chain

| Variable | Default | Description |
|----------|---------|-------------|
| `LSP_SERVERS` | (from route) | `;`-separated chain. Each entry: `command args...` |
| `LSP_COMMAND` / `LSP_ARGS` | (none) | Legacy primary server |
| `LSP_FALLBACK_COMMAND` / `LSP_FALLBACK_ARGS` | (none) | Legacy fallback server |
| `LSP_ROOT` | `cwd` | Workspace root path |
| `LSP_LANGUAGE` | (none) | Language identifier for config hashing |
| `LSP_PREFER` | (none) | Pre-seed method routing: `method=command,...` |
| `LSP_REPLACE` | (none) | Post-parse command substitution: `old=new,...` |
| `LSP_EMPTY_FALLBACK` | `textDocument/references,workspace/symbol` | Methods that fall through on empty results |
| `LSP_PROJECT_MARKERS` | `.git` | Project root detection markers |

### Tools

| Variable | Default | Description |
|----------|---------|-------------|
| `LSP_TOOLS` | `all` | Whitelist of enabled tools (comma list or `all`) |
| `LSP_EXCLUDE` | (none) | Blacklist of disabled tools |
| `LSP_GREP_MAX_FILES` | `2000` | Max files scanned by semantic grep |

### Warmup

| Variable | Default | Description |
|----------|---------|-------------|
| `LSP_WARMUP_PATTERNS` | (from route) | Glob patterns for warmup files (e.g. `*.py,*.pyi`) |
| `LSP_WARMUP_MAX_FILES` | `500` | Max files to warm per workspace folder |
| `LSP_WARMUP_EXCLUDE` | (none) | Extra directory names to exclude from warmup |

### Hooks & Gates

| Variable | Default | Description |
|----------|---------|-------------|
| `HSP_HOOKS` | `1` | Enable/disable hook event recording |
| `HSP_HOOK_CONTEXT` | `1` | Enable/disable file-scoped context injection in hooks |
| `HSP_REQUIRE_TICKET_FOR_EDITS` | (off) | Require active ticket before edits |
| `HSP_EDIT_GATE_SCOPE` | `workgroup` | Edit gate scope: `workgroup` or `agent` |
| `HSP_AUTHORITATIVE_BUILD` | `1` | Enable build dedup/batching |
| `HSP_BUILD_GATE_TIMEOUT` | `2m` | Timeout for implicit build gate wait |
| `HSP_BUILD_BATCH_TTL` | `30s` | TTL for cached build results |
| `HSP_BUILD_BATCH_WAIT_TIMEOUT` | `1800s` | Max wait for batch build result |

### Workgroup

| Variable | Default | Description |
|----------|---------|-------------|
| `HSP_WORKGROUP_ROOT` | (auto-discovered) | Override workgroup root (skips marker discovery) |
| `HSP_WORKGROUP_BOUNDARY` | (none) | Stop workgroup discovery at this path |
| `HSP_AGENT_ID` | (auto) | Stable agent identity for bus events |
| `HSP_BUS_DIR` | (derived) | Override bus JSONL storage directory |

### Integration

| Variable | Default | Description |
|----------|---------|-------------|
| `HSP_BABEL_BRIDGE` | (off) | Enable Babel bridge on the broker |
| `HSP_PROBE_CAPABILITIES` | (off) | Probe server capabilities at startup |
| `LSP_DEVTOOLS` | (off) | Expose broker to python-devtools |
| `LSP_DEVTOOLS_APP_ID` | `hsp-broker` | Override devtools app id |
| `LSP_DEVTOOLS_READONLY` | `1` | Devtools readonly mode |

## Standalone Usage

```bash
uv tool install hsp

# Explicit chain
LSP_COMMAND=ty LSP_ARGS=server hsp mcp
LSP_COMMAND=rust-analyzer hsp mcp
LSP_COMMAND=gopls LSP_ARGS=serve hsp mcp

# Router mode (auto-detects language)
hsp mcp
```

The MCP server speaks stdio through `hsp mcp`. Bare `hsp` shows the workgroup debug surface.

## For LSP Plugin Authors

`hsp mcp` is the MCP server; your plugin bundles it. Users install one plugin, get both native `lspServers` integration and the graph-oriented MCP tool set.

### 1. Declare the MCP server in `plugin.json`

```json
{
  "name": "ty-lsp",
  "version": "1.0.0",
  "lspServers": {
    "ty": { "command": "ty", "args": ["server"] }
  },
  "mcpServers": {
    "ty-lsp-extended": {
      "command": "uvx",
      "args": ["hsp", "mcp"],
      "env": {
        "LSP_SERVERS": "ty server;basedpyright-langserver --stdio"
      }
    }
  }
}
```

### 2. (Optional) Wire the redirect hook

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "LSP",
        "hooks": [{ "type": "command", "command": "hsp-redirect-hook" }]
      }
    ]
  }
}
```

### 3. Configure via env vars

Set in the `env` block of your `mcpServers` entry. See [Configuration Reference](#configuration-reference) for the full list.

**Chain behavior**: per-method. On `-32601` the next server in the chain is tried; first success is cached. `LSP_PREFER` pre-seeds the cache. `LSP_REPLACE` swaps commands post-parse.

### Adding a new language

Add a built-in route in `hsp.router` plus the plugin manifest's native `lspServers` entry. The old one-repo-per-LSP shape still works, but the preferred path is a single HSP plugin with routing inside the runtime.

## Harness Compatibility

HSP's hook system is designed for Claude Code but extends to other harnesses through Babel.

| Harness | Support | Hard Gates |
|---------|---------|------------|
| Claude Code | `wired` — full hooks, MCP, redirect | `wired` — edit denial, build wait |
| Codex CLI | `manual` — MCP/skills only | `open` — no apply_patch interception |
| Babel-supported adapters | `babel` — normalized lifecycle | `open` — needs per-harness mapping |
| Bridge-required (OpenCode, Amp, Kiro) | `bridge` — no implementation yet | `open` — needs deny replies |
| Unsupported (Aider, Roo Code, etc.) | `blocked` — manual MCP only | `blocked` — no hook surface |

Full matrix with 14 capability axes and 19 open tickets in [docs/harness-capability-matrix.md](docs/harness-capability-matrix.md).

## Feature Preservation Ledger

This ledger is the movement checklist for the Rust rewrite and Babel integration. If a behavior is listed here, it is intentional surface area unless a later design document explicitly deletes it.

### Distribution And Plugin Surface

- Package name is `hsp`, currently versioned as `0.15.x`, requiring Python 3.11+ and built with `uv_build`.
- Console entry points are `hsp`, `hsp-ty`, `hsp-csharp`, `hsp-redirect-hook`, `hsp-redirect-hook-ty`, `hsp-redirect-hook-csharp`, and `hsp-broker`.
- There are intentionally no `hsp-log`, `hsp-hook`, or `hsp-run` binaries; those are `hsp log`, `hsp hook`, and `hsp run` subcommands.
- Per-language MCP aliases exist because some harnesses dedupe MCP servers by command string.
- Three install shapes are live: Claude plugin under `.claude-plugin/`, root Codex plugin under `.codex-plugin/`, and bundled local Codex plugin under `plugins/hsp/.codex-plugin/`.
- `.mcp.json` and `plugins/hsp/.mcp.json` both launch HSP through `uvx --refresh --from git+https://github.com/holo-q/hsp hsp mcp` with router mode enabled.
- `hooks/claude.json` is the canonical Claude hook bundle; it invokes `hsp hook stdin <kind>` and uses `hsp-redirect-hook` for blocked raw LSP tool calls.
- Shipped skills are exactly `python-lsp`, `csharp-lsp`, `rust-lsp`, `workgroup-coordination`, `work-session`, and `work-ticket`.

### CLI Surface

- `hsp` / `hsp workgroup [LOCATIONS...]` prints root, source, workspace id, project root, gate policy, env, broker socket/log, JSONL log status, broker weather, and optional LSP status.
- `hsp mcp` runs the stdio MCP server.
- `hsp log <action>` supports `event`, `note`, `ask`, `reply`, `chat`, `ticket`, `journal`, `question`, `edit_gate`, `recent`, `settle`, `precommit`, `postcommit`, `weather`, `presence`, `workgroup`, `status`, and `hook` alias.
- `hsp hook stdin <kind>` drains Claude hook JSON, records events, injects context, applies edit/build gates, and is disabled by `HSP_HOOKS=0`.
- `hsp run -- <command>` waits for the build gate, runs the command, records `test.ran`, and supports `--timeout`, `--kind`, `--files`, `--symbols`, `--message`, and `--no-log`.
- `hsp watch [LOCATIONS...]` renders recent bus weather with `--global`, `--exact`, `--once`, `--limit`, `--interval`, and broker auto-start controls.
- `hsp global` reports singleton broker state, bus counters, devtools, Babel bridge state, sessions, routes, handlers, clients, and split-broker warnings.

### Broker And Wire Runtime

- Broker socket resolution is `HSP_BROKER_SOCKET`, then `$XDG_RUNTIME_DIR`, then `/run/user/<uid>`, then `/tmp/hsp-broker-<user>/` with private directory mode.
- Broker log resolution is `HSP_BROKER_LOG`, then `$XDG_STATE_HOME/hsp/broker.log`, then `~/.local/state/hsp/broker.log`.
- The broker unlinks stale sockets, handles SIGINT/SIGTERM, and stops LSP clients, devtools, and the Babel bridge on shutdown.
- JSONL wire messages are compact and sorted; errors include `unknown_method`, `invalid_request`, `invalid_params`, `transport`, `internal`, `lsp:<code>`, `broker_unreachable`, and `not_connected`.
- `HSP_BROKER_IDLE_TTL_SECONDS` defaults to 14400 seconds; `0` disables idle eviction.
- `BrokerClient.connect_or_start()` spawns `python -m hsp.broker`, detaches it, writes the broker log, polls for up to 10 seconds, and uses a 2 second connect timeout.
- `SessionKey(root, config_hash)` dedupes LSP sessions; `config_hash` is a 12-character SHA-256 prefix. The bus is not sharded by config hash.
- Broker methods include core, session, LSP, render memory, and the full bus method set listed above.
- `LSP_DEVTOOLS=1` registers `broker`, `bus`, `registry`, and `lsp` with python-devtools; readonly mode defaults on.

### Semantic MCP Tools

- `_ALL_TOOLS` contains 25 tools: `diagnostics`, `grep`, `symbols_at`, `symbol`, `show_definition`, `show_declaration`, `show_type`, `show_implementation`, `show_origins`, `refs`, `outline`, `rename`, `move`, `fix`, `calls`, `types`, `path`, `confirm`, `session`, `log`, `ticket`, `journal`, `ask`, `chat`, and `memory`.
- Registry keys control enablement and capability gating; public MCP names come from the callable names, for example `grep` is exposed as `lsp_grep` while `show_definition` stays `show_definition`.
- Tool enablement is controlled by `LSP_TOOLS=all|comma-list`, `LSP_EXCLUDE`, and `LSP_DISABLED_TOOLS`; `HSP_PROBE_CAPABILITIES=1` can hide tools unsupported by the active chain.
- Every broker-wrapped result may include header telemetry: selected server/method, newly started server, newly added workspace, drained bus messages, and heartbeat.
- Semantic targets accept graph indices, aliases, bare `Lxx` from the latest graph, `file:Lxx`, unique basenames, and explicit `file_path` plus `symbol` or `line`.
- `lsp_grep` scans text candidates first, asks LSP what each occurrence binds to, groups by definition identity, marks unresolved text-only buckets, ignores comments, and enforces `LSP_GREP_MAX_FILES`.
- `lsp_outline` batches files or glob patterns into compact line-addressable breadcrumbs.
- `lsp_diagnostics` accepts comma-separated paths or glob patterns.
- `lsp_calls` and `lsp_types` run prepare-once hierarchy flows, expand by direction, and record returned edges into semantic navigation context.
- `lsp_path` searches bounded witness paths through call hierarchy with hop, edge, path, via, and exclude controls.
- `show_origins` is the union of definition, declaration, type definition, and implementation lookup.

### Mutation And Preview Semantics

- Mutating tools preview first and stage candidates into a pending book; `lsp_confirm` applies one candidate.
- Pending stages can be addressed by index or named handle so parallel agents do not overwrite each other's staged previews.
- Workspace edits support `changes`, `documentChanges`, create, rename, and delete operations, with UTF-16 position handling.
- `lsp_rename` stages a workspace rename edit.
- `lsp_fix` renders diagnostics as `(d0)`, `(d1)`, forwards either one diagnostic or all diagnostics with `diagnostic_index=-1`, filters by code-action kind prefix, and stages only edit-backed actions. Command-only actions are visible but not applicable.
- `lsp_move` supports single file moves, symbol-to-file moves, and batch `from=>to` pairs; one confirm applies import rewrites and filesystem moves atomically.
- Python move fallback rewrites ordinary imports when basedpyright returns too little from `workspace/willRenameFiles`.

### Language Routing And LSP Chain

- Built-in routes are Python, C#, and Rust, selected by extension and deepest non-`.git` project marker unless `HSP_ROUTE` forces a route.
- Route probing reads URI, `route_path`, path, or file-operation created/renamed/deleted payloads.
- `LSP_SERVERS` defines a semicolon-separated chain; legacy `LSP_COMMAND`/`LSP_ARGS` plus fallback env vars still work.
- `LSP_REPLACE` substitutes parsed commands; `LSP_PREFER` pre-seeds per-method routing; `LSP_EMPTY_FALLBACK` permits empty-result fallthrough for selected methods.
- Per-method cache maps a request method to the first successful server, including a cached miss when no server supports it.
- `workspace/willRenameFiles` gets a slow 300 second timeout; other LSP requests default to 30 seconds.
- Before each request, HSP resyncs open documents, auto-adds workspace folders by project markers, and ensures the target document is open.
- Missing language-server binaries fail early with install-hint LSP errors.
- File watching uses `watchdog`, recursive `.py`/`.pyi` watches, 100ms debounce, `didChangeWatchedFiles`, open-document `didChange`, and broad generated/cache directory excludes.

### Bus, Workgroups, And Gates

- Bus storage has two live layouts: broker mode under `$XDG_STATE_HOME/hsp/bus/<workspace-id>/events.jsonl` and direct mode under `<workspace>/tmp/hsp-bus/events.jsonl`; `HSP_BUS_DIR` overrides. A legacy `<workspace>/tmp/hsp-bus.jsonl` file is still written for backcompat.
- Bus events carry `seq`, `event_id`, `kind`, timestamp, workspace id/root, agent/client/session/task ids, git head, dirty hash, scope, message, metadata, question id, and `schema_version=1`.
- Messages are clipped to 8 KiB and marked truncated.
- Canonical event kinds include lifecycle, prompt, task, notification, tool, confirmation, compaction, edit, file/symbol touch, git, test, ticket, note/chat/question, bus close, and `babel.event`.
- Hook adapter kinds such as `permission.request`, `session.end`, `stop.failure`, and `compact.after` are hook inputs, not canonical `BusEventKind` values unless explicitly normalized.
- Event aliases are accepted on wire: for example `prompt.start`, `session.started`, `session.ended`, `stop`, `pre_tool`, `post_tool`, `pre_compact`, `subagent_stop`, `test.result`, `git.commit`, and `git.push`.
- Empty `BusScope` is a wildcard. File overlap is fuzzy: exact path, basename suffix, and prefix containment all count.
- Workgroups discover `workgroup.toml` and `.hsp/workgroup.toml`, support boundary and root overrides, and infer project roots from route markers plus package/workspace markers.
- `workgroup.toml` may carry name, level, icon/glyph/symbol/mark, color/fg/foreground, and ANSI fields.
- The Rust rewrite should treat the standalone orgmap / `hsp-workgroup` library as the authority for workgroup-map parsing and discovery instead of reimplementing the Python-local mapper.
- Tickets are one-per-agent, keyed by agent/client/session identity. Empty ticket message releases; same trimmed message in the same workspace coalesces; starting a new ticket clears stale build waiters.
- Ticket scope accumulates files, symbols, aliases, projects, and project roots across joins.
- Build gate returns `clear`, `all_waiting`, or `active_tickets`, blocks unknown-scope tickets conservatively, and keys authoritative build batches by workspace plus sorted projects.
- Authoritative build batching writes under `tmp/hsp-build-batches/`, uses exclusive locks, caches for `HSP_BUILD_BATCH_TTL`, waits up to `HSP_BUILD_BATCH_WAIT_TIMEOUT`, truncates captured output, and suppresses duplicate Bash executions through hook denial payloads.
- Build command classification recognizes common runners and checkers, unwraps `uv run`, `poetry run`, `pipenv run`, and `npx`, and has tool-specific specs for major ecosystems.
- Edit gate is opt-in via `HSP_REQUIRE_TICKET_FOR_EDITS=1`, supports workgroup or agent scope, and denies edits through Claude hook permission payloads.
- Questions use `Q...` ids, default to `3m`, accept `ms`, `s`, `m`, and `h` timeout units, settle lazily, and emit `bus.closed` with digest metadata. Late replies are recorded with `metadata.late=true` and do not rewrite the digest.
- Presence is keyed by client, agent, or session id; active is under 60 seconds, asleep is 60+ seconds, stale entries prune after 600 seconds, and durable events imply heartbeat.
- Hook context injection handles read/edit/tool boundaries, extracts file paths from tool input shapes, normalizes command status, and fails soft.
- `hsp watch` uses global, tree, exact, or explicit observation modes; `journal` and `recent` settle expired questions before rendering.

### Babel Bridge Preservation

- `HSP_BABEL_BRIDGE=1` starts a broker-side subscriber to `$XDG_RUNTIME_DIR/babel.sock`, falling back to `/tmp/babel-<uid>.sock`.
- The bridge subscribes to `window_added`, `window_removed`, `pane_focused`, `pane_unfocused`, `session_matched`, `session_updated`, `session_state_changed`, `activity_pulse`, `session_started`, `tool_started`, `tool_completed`, `notification_received`, `subagent_completed`, `transcript_compacting`, and `daemon_shutdown`.
- Native events normalize to bus events: agent/window starts, session starts, heartbeats, tool before/after, notifications, subagent stops, compaction, and session stops.
- Every bridged event carries `metadata.source=babel` and `metadata.native_event`.
- Babel agent ids are derived from agent kind, session id, pane address, or native event fallback.
- Workspace root comes from event project, event cwd, `LSP_ROOT`, then process cwd.
- Reconnect is best-effort with a 5 second delay; HSP keeps working when Babel is absent.

### Rewrite Watchpoints

- Consolidate the two bus implementations (`AgentBus` and `BusJournal`/`BusLog`/`BusRegistry`) without changing wire behavior.
- Move workgroup/orgmap semantics to the standalone orgmap / `hsp-workgroup` crate boundary; HSP should consume the map, not own another divergent parser.
- Resolve the workspace-id hash divergence in the bus stack deliberately; do not accidentally split old and new workgroups.
- Preserve hook-kind normalization so adapter inputs do not leak as invalid canonical event kinds.
- Preserve the distinction between `E{seq}` event handles and raw sequence numbers.
- Preserve prompt `.end` mapping to `session.stop`.
- Preserve append fsync behavior for durable JSONL events; heartbeat must stay non-durable.
- Preserve recent-render differences intentionally or delete them explicitly: live `AgentBus.recent` and `BusJournal.recent` do not currently skip own events the same way.
- Update `hsp-redirect-hook` alongside MCP tools; it must name all public replacement tools including declaration, type, and implementation lookups.
- Keep build and edit gates as hook-denial semantics, not just advisory bus rows.

## Design Documents

| Document | Purpose |
|----------|---------|
| [docs/tool-surface.md](docs/tool-surface.md) | Agent-first tool surface design, raw-to-workflow cut map, implementation waves |
| [docs/agent-bus.md](docs/agent-bus.md) | Agent bus design: events, tickets, questions, presence, hook recipes |
| [docs/rendering.md](docs/rendering.md) | Output rendering contract: row shapes, graph handles, preview format |
| [docs/render-memory.md](docs/render-memory.md) | Render memory: alias grammar, epochs, compression levels, guardrails |
| [docs/lsp-path.md](docs/lsp-path.md) | Bounded witness path operator: edge families, search policy, output contract |
| [docs/lsp-grep.md](docs/lsp-grep.md) | Semantic bucketizer: text search + LSP binding, breadcrumb format |
| [docs/broker.md](docs/broker.md) | Broker daemon design |
| [docs/agent-tool-roadmap.md](docs/agent-tool-roadmap.md) | Future tools: what-if, witness, impact, test targets, dead code |
| [docs/harness-capability-matrix.md](docs/harness-capability-matrix.md) | Cross-harness support matrix and open tickets |
| [docs/csharp-dapper-fixture.md](docs/csharp-dapper-fixture.md) | C# integration test setup |

## Context

Built to address [claude-code#40282](https://github.com/anthropics/claude-code/issues/40282) — Claude Code's native LSP tool is missing operations and buggy for some that it does implement. HSP bridges the gap while evolving toward a graph-operator interface that goes beyond raw LSP mirroring.

## License

MIT
