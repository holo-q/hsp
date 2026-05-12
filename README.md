# HSP

Rust rewrite of the Harness Server Protocol.

The Python implementation has been moved to [references/hsp-py](references/hsp-py) and remains the feature reference during the rewrite. It is kept as a reference repository at commit `a7af7b4` so the Rust crate can move without flattening the old history. Its [feature preservation ledger](references/hsp-py/README.md#feature-preservation-ledger) is the migration checklist.

Workgroup and orgmap semantics belong to the standalone `orgmap` / `hsp-workgroup` library. HSP should consume that map boundary instead of reimplementing marker parsing, hierarchy discovery, observation roots, color/icon identity, or workspace naming.

## Architecture

- `crates/hsp-wire` owns serializable DTOs and wire invariants. It must stay light: no async runtime, storage, LSP, TUI, or parser stacks.
- `crates/hsp-store` owns persistence and row mapping only. JSONL append/replay, workspace id hashing, and bus directory policy live here, not in bus policy.
- `crates/hsp-org` owns HSP's facade over `orgmap`; workgroup parsing and hierarchy discovery remain outside HSP.
- `crates/hsp-session` owns broker session identity and registry state. It does not start language servers.
- `crates/hsp-bus` owns agent-bus policy over `hsp-wire` events: sequence handles, truncation, scope filtering, event wire views, ticket lifecycle transitions, questions/replies/settle digests, decaying presence, build gates, and edit gates. It can rehydrate from replayed events but does not know where they came from.
- `crates/hsp-broker` owns request dispatch and runtime orchestration. The current slice is in-process only and exposes bus append/recent/journal/weather/precommit/postcommit, question/reply/settle/chat routing, heartbeat/presence, and ticket/build/edit gates; socket serving, persistence wiring, and LSP supervision come later.
- `src/lib.rs` is the root facade for callers that want the integrated HSP surface.
- `src/main.rs` is currently a probe binary for checking which workgroup stack HSP sees from a path.
- `references/hsp-py/` is the Python reference repo at the last pre-move commit.

## Parity Path

1. Keep `hsp-wire` data-first and preserve JSON shape before adding runtime behavior.
2. Grow `hsp-bus` from pure journal policy into tickets, questions, presence, and gates while keeping storage behind `hsp-store`. Ticket transitions and question closes now emit durable-shaped event rows through the same journal path as ordinary bus events, and every durable event updates the derived presence view.
3. Wire `hsp-store` into broker/runtime after the in-memory bus contract is pinned; append/recent/journal shapes should not change when JSONL replay lands. The store now owns `workspace_id_for`, `bus_dir_for`, and `log_path_for`; the bus owns `from_events` rehydration.
4. Port LSP routing/session management as a runtime crate, not as DTO residue.
5. Preserve every behavior listed in the Python reference ledger or delete it by explicit design note.
