# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

Within the current five-item batch, items are implemented one at a time in listed order; everything else stays in `plan.md` until its turn.

Current batch (selected 2026-10-08, in plan.md order + severity):

1. **DB-02 — 123-table Help Scout mirror schema** (Partial, Major). Complete the 123-table Help Scout mirror schema (add missing tables), keeping documented renames (threads->conversation_threads, segments->saved_segments, support_graph_edges->graph_edges, customer_memories->customer_memory, conversation_events->activity_events, knowledge_candidates->knowledge_gap_candidates). Audit diff: port boots 120 of MAIN's 123 migration tables; missing `docs_articles` (legacy `docs` name), `encrypted_sync_log` (lazy ensure never in the boot chain), `issue_cluster_conversations` (undocumented `issue_cluster_members` shape); MAIN's runtime `schema_migrations` maps to the port's documented `_migrations` (DB-01).
2. **DB-04 — threads actor model (user/customer/system split)** (Divergent, Major). Restore MAIN's threads actor model (from_type + created_by_user_id/created_by_customer_id/created_by_system_user_id 3-way split) instead of the collapsed actor_id+actor_type, or map it at query boundaries. Includes the thread_type->type and body->body_text reference renames.
3. **DB-06 — FK enforcement + pragma parity (WAL, foreign_keys, busy_timeout)** (Partial, Major). Declare foreign keys on base mirror tables (currently zero FKs on conversations/customers/users/mailboxes/conversation_threads and ten more) to match MAIN's enforcement; keep pragma parity (WAL, foreign_keys, busy_timeout — already enforced by db::open, to be verified by test).
4. **OP-04 — Automation-approvals tile (parked jobs count)** (Divergent, Major). Automation-approvals operations tile always returns 0; count parked/awaiting-approval jobs (automation_approvals) as MAIN's tile does. Root cause: the tile and the notification sweep count `status IN ('queued','parked')` while the park path writes status `awaiting_approval` (MAIN's own stale-vocabulary bug, ported verbatim); the port's `automation_approvals` pending rows are never counted either.
5. **DB-03 — conversations table shape (number UNIQUE, FK names, 47 cols)** (Divergent, Major, L). Restore MAIN column names (mailbox_local_id/assignee_local_id/customer_local_id) + UNIQUE(number) via new migration; rewrite ported SQL (the ~70 SQL sites touching the renamed columns).
