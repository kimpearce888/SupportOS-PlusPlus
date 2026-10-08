# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

Within the current five-item batch, items are implemented one at a time in listed order; everything else stays in `plan.md` until its turn.

Current batch (started 2026-10-08, `continue` signal received):

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
| 69 | BU-02 | Desktop release pipeline (win/mac/linux matrix, MSI/NSIS/DMG/AppImage) | Partial | Major | L | - | Add Windows/macOS targets, icons (.ico/.icns), release automation |
| 70 | M16 | Multi-step writes untransacted in side threads / incident features | Defect | Major | M | T16 M16 | Wrap multi-step writes in transactions (side_threads.rs:179-303; intelligence_features.rs:700-830) to avoid partial states. |
| 71 | M28 | run_ai holds the only DB mutex across the LM Studio call | Defect | Major | M | T16 M28 | Move AI runs off the global AppState mutex (spawn_blocking + per-call connection, or apply the LM Studio timeout) in routes/ai.rs:118-140; one hung AI call currently freezes all requests and workers. |
| 72 | DB-01 | 16 forward-only migrations, versioned, transactional | Divergent | Major | L | - | Consolidate boot batches into versioned transactions; stop fabricating MAIN history rows; record real applied versions |
| 73 | CL-08 | Side-thread audit trail on mutations | Missing | Major | S | - | Write audit_log rows on side-thread mutations |
