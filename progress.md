# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

Within the current five-item batch, items are implemented one at a time in listed order; everything else stays in `plan.md` until its turn.

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
| 54 | ME-01 | Composed customer memory profile (9 sections + freshness) | Missing | Major | L | M6 | Compose profile at read time (issue history, outcomes, interaction, AI entries, freshness, quarantined) |
| 55 | GR-01 | Derived edge layer (~24 read-time branches) | Missing | Major | XL | M4 | Port read-time derived-edge layer (or populate edges on sync) |
| 56 | UI-17 | Notification center (tabs, filters, prefs, mention queue) | Partial | Major | M | M24 | Wire mark-read/read-all to API; persist prefs via PUT; parse MAIN field names; mention queue; filters |
| 57 | UI-19 | Sync health page (sync actions, checkpoints, health cards, webhook register) | Stub | Major | M | M25 | Port sync actions, checkpoints, runs, health cards, webhook register via API |
| 58 | UI-21 | Onboarding (6-step wizard + live checks) | Divergent | Major | M | - | 6-step wizard with live LM Studio/Qdrant checks |
