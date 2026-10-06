# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

| ID | Item | Status | Severity | What needs to be fixed |
|---|---|---|---|---|
| SY-09 | Priority API queue (concurrency 2, priority sort) | Missing | Major | Implement priority queue with concurrency 2 wrapping provider calls (audit M13: 'api_queue' is stats counters only, helpscout_real.rs:279-299). |
| AI-14 | Report narrative (facts-only prompt) | Divergent | Major | Route /api/reports/narrative to the existing ai_pipeline implementation. |
| AI-21 | /api/analytics/ai draft stats | Divergent | Major | Point /api/analytics/ai at the same implementation as /api/ai/analytics. |
| AN-04 | Why-contacting report | Stub | Major | Implement why-contacting computation + MAIN response shape (audit M3). |
| AN-05 | Top questions report | Stub | Major | Implement top-questions from FTS/knowledge queries (audit M3). |
