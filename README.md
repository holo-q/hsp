# HSP

Rust rewrite of the Harness Server Protocol.

The Python implementation has been moved to [references/hsp-py](references/hsp-py) and remains the feature reference during the rewrite. It is kept as a reference repository at commit `a7af7b4` so the Rust crate can move without flattening the old history. Its [feature preservation ledger](references/hsp-py/README.md#feature-preservation-ledger) is the migration checklist.

Workgroup and orgmap semantics belong to the standalone `orgmap` / `hsp-workgroup` library. HSP should consume that map boundary instead of reimplementing marker parsing, hierarchy discovery, observation roots, color/icon identity, or workspace naming.

## Architecture

- `crates/hsp-wire` owns serializable DTOs and wire invariants. It must stay light: no async runtime, storage, LSP, TUI, or parser stacks.
- `crates/hsp-store` owns persistence and row mapping only. JSONL append/replay, workspace id hashing, and bus directory policy live here, not in bus policy.
- `crates/hsp-org` owns HSP's facade over `orgmap`; workgroup parsing and hierarchy discovery remain outside HSP.
- `crates/hsp-protocol` owns broker path and environment policy: socket path resolution, broker log path resolution, and idle TTL defaults. It is pure and has no runtime dependency.
- `crates/hsp-client` owns synchronous JSONL-over-UnixStream requests and broker auto-start.
- `crates/hsp-daemon` owns the `hsp-broker` Unix socket runtime around `BrokerCore`; it does not own dispatch semantics.
- `crates/hsp-session` owns broker session identity and registry state. It does not start language servers.
- `crates/hsp-bus` owns agent-bus policy over `hsp-wire` events: sequence handles, truncation, scope filtering, event wire views, ticket lifecycle transitions, questions/replies/settle digests, decaying presence, build gates, and edit gates. It can rehydrate from replayed events but does not know where they came from.
- `crates/hsp-broker` owns request dispatch and runtime orchestration. The current slice exposes bus append/recent/journal/weather/precommit/postcommit, question/reply/settle/chat routing, heartbeat/presence, and ticket/build/edit gates. It lazily replays per-workspace JSONL logs through `hsp-store`, persists every durable bus row, and leaves heartbeat live-only.
- `src/lib.rs` is the root facade for callers that want the integrated HSP surface.
- `src/main.rs` / `src/cli.rs` are the CLI adapter. They keep argument parsing and broker-friendly defaults out of the core crates while exposing workgroup probes, broker lifecycle commands, `hsp log ...` bus actions, stdin hook recording, build-gated `hsp run`, `hsp watch`, and broker-global status.
- `references/hsp-py/` is the Python reference repo at the last pre-move commit.

## Parity Path

1. Keep `hsp-wire` data-first and preserve JSON shape before adding runtime behavior.
2. Grow `hsp-bus` from pure journal policy into tickets, questions, presence, and gates while keeping storage behind `hsp-store`. Ticket transitions and question closes now emit durable-shaped event rows through the same journal path as ordinary bus events, and every durable event updates the derived presence view.
3. Wire `hsp-store` into broker/runtime after the in-memory bus contract is pinned; append/recent/journal shapes should not change when JSONL replay lands. The store now owns `workspace_id_for`, `bus_dir_for`, and `log_path_for`; the bus owns event/ticket rehydration; the broker owns lazy workspace replay and durable append timing.
4. Add daemon/client socket runtime on top of `hsp-protocol`, keeping socket serving and sync client transport outside `hsp-broker` dispatch. `hsp-client` and `hsp-daemon` now cover the synchronous client, daemon socket runtime, and `hsp-broker` binary skeleton.
5. Restore the user-facing bus surface through the Rust CLI. `hsp log <action>` now reaches broker bus methods for event/note/ask/reply/chat/ticket/journal/question/recent/settle/precommit/postcommit/weather/presence/status/build_gate/edit_gate and supplies stable CLI agent/client defaults. `hsp hook stdin <kind>` records hook payloads as bus events, `hsp run -- <command>` waits on the build gate and records `test.ran`, and `hsp watch --once` / `hsp global` provide quick broker visibility while the richer Python watch formatting is still being ported.
6. Port LSP routing/session management as a runtime crate, not as DTO residue.
7. Preserve every behavior listed in the Python reference ledger or delete it by explicit design note.
