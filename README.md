# HSP

Rust rewrite shell for the Harness Server Protocol.

The Python implementation has been moved to [references/hsp-py](references/hsp-py) and remains the feature reference during the rewrite. It is kept as a reference repository at commit `a7af7b4` so the Rust crate can move without flattening the old history. Its [feature preservation ledger](references/hsp-py/README.md#feature-preservation-ledger) is the migration checklist.

Workgroup and orgmap semantics belong to the standalone `orgmap` / `hsp-workgroup` library. HSP should consume that map boundary instead of reimplementing marker parsing, hierarchy discovery, observation roots, color/icon identity, or workspace naming.

## Current Shape

- `src/lib.rs` exposes the first Rust-side workspace discovery shell.
- `src/main.rs` is a tiny probe binary for checking which workgroup stack HSP sees from a path.
- `references/hsp-py/` is the Python reference repo at the last pre-move commit.

## Next Movement

1. Port the broker wire model and event bus types as data-first Rust modules.
2. Fold workgroup discovery through `orgmap` before touching bus scope logic.
3. Preserve every behavior listed in the Python reference ledger or delete it by explicit design note.
