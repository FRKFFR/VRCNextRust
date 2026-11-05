# STATUS

| Milestone | State | Session | Branch / PR | Outcome | Open issues |
|---|---|---|---|---|---|
| Setup | done | lead | main | repo, RULES.md, architecture, orchestrator | — |
| M1 Foundations | M1b in review | m1b-coder | agent/m1b-vrc-api-client-read-rules-md-first-m-mw3qpgur | M1b vrc-api client: auth + 2FA, CookieVault (keyring/memory), token-bucket rate limiter + retry, models, wiremock tests | clipboard-win 5.4.3 via eframe accepted as pin exception; re-pin to 5.4.2 after 2026-11-06 |
| M2 Real-time | M2b in review | m2b-coder | agent/m2b-desktop-ui-for-real-time-friends-rea-mw3vxzls | M2b desktop UI: egui login/2FA/main screens, friend sections + search, friends' worlds dashboard with lazy world-name resolution (`GET /worlds/{id}`, 10-min failure cache) | — |
| M3 Local data | in review | m3-coder | agent/m3-local-data-sqlite-vrchat-log-parser-t-mw3zy1r8 | vrc-logs log parser/tailer, vrc-store SQLite repos (timeline, friend history, sessions, stats), vrc-core commands + store thread, egui data tabs, cli timeline/stats/tail-logs | — |
| M4 Social actions | not started | | | | |
| M5 Worlds, groups, avatars | not started | | | | |
| M6 Media | not started | | | | |
| M7 Launcher and tools | not started | | | | |
| M8 Optional | not started | | | | |

## Notes
- Orchestration tooling lives outside this repo.
