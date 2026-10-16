# Ledger modal evidence

2026-10-15 · `myr-a1i` · Codex `gylleus/ledger-modal`.

Native egui captures with synthetic entries and the farm fixture, at 1440×960 points.
The before captures use the Ledger UI from `afb887b` and its 60/40 dock split. The after
captures use the modal after integrating `main` (including the shared Markdown renderer,
Lorekeeper controls, resting room and click-to-focus work). Other panes are fixture labels;
no real chat, Ledger entries, app services, or maintenance runs were used for these images.
Search results and the entry body are preloaded fixtures; transport is verified separately.

| State | Before: dock tab | After: modal |
| --- | --- | --- |
| Opening | [Before](before-opening.png) | [After](after-opening.png) |
| Search results | [Before](before-search.png) | [After](after-search.png) |
| Entry open | [Before](before-entry.png) | [After](after-entry.png) |

Additional captures: [light theme](after-light.png), [custom art palette and UI accent](after-custom.png).
The custom capture uses `hex:264653-2a9d8f-e9c46a` for the farm and a green UI selection accent.

Automated checks cover chamber clicks preserving selection and dock state; initial search
focus; arrow/Enter/Escape handling; reopening with query, entry and results scroll intact;
stale background replies; and removing the old Ledger split without resetting other panes.
Existing local search tests cover title/body matches and read-only file access. The live
`cargo test real_ledger_search --lib -- --ignored` check also passed against the local
basic-memory service. Full gates: `cargo fmt --check`, `cargo clippy --all-targets` (only the
existing theme warning), and `cargo test` (1691 library tests and 4 integration tests passed).
