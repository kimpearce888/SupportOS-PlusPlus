# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

| ID | Item | Status | Severity | What needs to be fixed |
|---|---|---|---|---|
| M20 | SIGTERM not handled (ctrl_c only) | Defect | Major | Handle SIGTERM alongside ctrl_c for graceful shutdown (crates/core/src/http/server.rs:1307-1312 vs MAIN index.ts:88-89). |
| SY-07 | OAuth (authorize-url, callback, refresh, client-credentials, disconnect, status) | Partial | Major | Implement or remove the Tauri loopback OAuth/webhook receiver (loopback.rs binds then drops; handlers are {ok:true,todo:M2} stubs); delete the dead exchange_code legacy path (oauth.rs:200-247). |
| TH-09 | bodyLimit 20MB (attachments) | Missing | Major | DefaultBodyLimit' crates/core/src (0 hits) |
| OR-03 | Campaign reply tracking (refreshReplies, reply_rate) | Missing | Major | Call reply scan in campaign report (refreshReplies port) |
| WK-05 | Background automatic AI (process_new_ticket on new/reply) | Missing | Major | Enqueue analyze_ticket on new/reply (sync hook + automation engine) and handle in worker loop |
