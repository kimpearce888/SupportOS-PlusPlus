# completed.md — completed and verified items

Items land here only after the fix has been implemented **and** verified (rule.md, Rule 5).

| ID | Item | Status | Severity | What was changed | How it was verified | Completed (UTC) |
|---|---|---|---|---|---|---|
| UI-23 | Packaged app API connectivity (webview origin allowed by CORS) | Divergent | Blocker | CORS allowlist in crates/core/src/http/server.rs: added http://tauri.localhost, https://tauri.localhost, tauri://localhost (packaged webview) and http://127.0.0.1:1420 + http://localhost:1420 (tauri.conf.json devUrl), with a stack-adaptation comment. New integration test crates/core/tests/cors_origins.rs boots the real loopback server and live-probes CORS. | cargo test -p supportos-plusplus-core --test cors_origins: 1 passed - all 9 allowlisted origins (incl. the 5 new ones) echo ACAO; foreign origin gets 200 with NO ACAO (MAIN deny-without-throw); no-Origin request gets no ACAO; preflight from http://tauri.localhost answered with POST in allow-methods. cargo test --test e2e_demo_boot: 1 passed (server-boot regression). cargo clippy -p supportos-plusplus-core --lib --tests: exit 0, no warnings. cargo fmt --check: clean. Commit ac0839b. | 2026-10-05 11:00 UTC |

