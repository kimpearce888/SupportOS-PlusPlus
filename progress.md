# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|---|
| 4 | AI-19 | Interaction routes (GET card/evidence/profile, refresh) | Divergent | Critical | M | C7 | Fix GET queries to real schema; create evidence table; implement refresh; serve profile from signals |
| 5 | SEC-02 | HTML sanitizing (40 tags, attr map, schemes, css clip, a-hardening) | Partial | Critical | S | T11 inv.8 | Strip protocol-relative URLs (UrlRelative::Custom deny) ; remove whole img on data:text/html |
| 6 | SY-06 | Real provider wire protocol (inboxId, _links cursor, HAL docs, /v3/system-users, pagination loops) | Divergent | Critical | M | C5 | Use inboxId=, _links.next.href cursor, /v3/system-users, HAL _embedded parsing, page loops for users/tags/orgs/workflows |
| 7 | WK-03 | Job-kind executor coverage (~34 kinds) | Partial | Critical | L | C6 | Add missing job handlers (AI family, bulk ops, attachments, outreach send/reconcile, refresh_report) or stop enqueuing unrunnable kinds |
| 8 | SY-05 | Mirror write fidelity (tags/fields/emails/properties/recipients/ratings) | Divergent | Critical | L | C8 | Persist emails/properties/phones on customer upsert; store thread recipients/attachments |
