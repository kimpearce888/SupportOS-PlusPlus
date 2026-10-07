# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

Within the current five-item batch, items are implemented one at a time in listed order; everything else stays in `plan.md` until its turn.

Current batch (started 2026-10-08):

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|---|
| 64 | UI-07 | Issues page (5 tabs: radar/clusters/known/gaps/reuse) | Stub | Major | L | M26 | Port 5 tabs (radar/clusters/known/gaps/reuse) calling existing APIs |
| 65 | UI-14 | Reports page (10 tabs) | Partial | Major | L | - | Port 6 missing tabs (SLA/why-contacting/intelligence/HS reports/definitions/release) |
| 66 | UI-02 | Inbox page (~51 subfeatures: views, filters, bulk, snooze, schedule, attachments, tags/fields editors, AI draft composer, audit, activity) | Partial | Major | XL | M8 | Port ~30 missing subfeatures (views/filterbar/bulk/snooze/schedule/attachments/editors/audit/activity/draft composer/saved replies/cc/bcc/Cmd+Enter/close-confirm/pagination/URL state/assignee picker) |
| 67 | BU-01 | CI pipeline (lint+typecheck+build+test+smoke, Node 20) | Missing | Major | S | M12 | Add CI: fmt+clippy+test+build+smoke on push/PR |
| 68 | AI-23 | AI evaluation mode + golden set | Partial | Major | M | M29 | Create golden_test_set + seed; restore evaluation runs |
