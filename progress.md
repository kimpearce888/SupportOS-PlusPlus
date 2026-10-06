# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

| ID | Item | Status | Severity | What needs to be fixed |
|---|---|---|---|---|
| AN-01 | Dashboard (20 fields incl. by_tag/agent/team/channel/daily/mailbox_comparison/ratings/avg times) | Divergent | Major | Compute all 20 dashboard fields from mirror; accept days + mailboxIds + channel params (audit M1). |
| AN-09 | Metric definitions endpoint | Missing | Major | Serve the 11 seeded metric definitions; release-correlation + release-events CRUD landed with AN-08/AN-10 — verify at parity (audit M3). |
| AN-12 | Report builder run (metrics->SQL, parameterized, dimensions) | Partial | Major | Fix metric SQL to MAIN semantics (published+not-deleted, customer kind, channel via source_type) (audit M2). |
| AN-15 | Support health (no-score design, metrics+flags+incidents) | Divergent | Major | Replace verdict with MAIN's metrics+flags+incidents model (audit M22). |
| AC-04 | Events endpoint (actor names, metadata, counts) | Partial | Major | Join actor names, metadata, counts, limit param. |
