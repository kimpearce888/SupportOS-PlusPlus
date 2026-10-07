# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

Within the current five-item batch, items are implemented one at a time in listed order; everything else stays in `plan.md` until its turn.

**Current batch (IS-03, TL-02, TS-03, VW-03, GR-03)**

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|---|
| 49 | IS-03 | Engineering refs + support cases (from-conversation capture) | Stub | Major | M | - | Implement refs CRUD + support-case capture from conversation |
| 50 | TL-02 | Customer/organization timeline reads (kind counts, filters) | Divergent | Major | M | - | Serve event-kind timeline for customers with filters |
| 51 | TS-03 | Transition history + per-state lifecycle serving | Missing | Major | M | - | Serve transition history + per-state lifecycle in conversation detail |
| 52 | VW-03 | Apply savedViewId/aiAttribute/filter params to inbox list | Missing | Major | M | M7 | Compile saved views + AI-attribute filters into the list query |
| 53 | GR-03 | Neighbors/subgraph(BFS depth<=2)/search/stats | Stub | Major | L | M4 | Implement bounded BFS subgraph, per-kind stats, per-kind capped search |
