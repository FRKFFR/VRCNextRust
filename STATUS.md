# STATUS

| Milestone | State | Session | Branch / PR | Outcome | Open issues |
|---|---|---|---|---|---|
| Setup | done | lead | main | repo, RULES.md, architecture, orchestrator | — |
| M1 Foundations | M1b in review | m1b-coder | agent/m1b-vrc-api-client-read-rules-md-first-m-mw3qpgur | M1b vrc-api client: auth + 2FA, CookieVault (keyring/memory), token-bucket rate limiter + retry, models, wiremock tests | clipboard-win 5.4.3 via eframe accepted as pin exception; re-pin to 5.4.2 after 2026-11-06 |
| M2 Real-time | M2b in review | m2b-coder | agent/m2b-desktop-ui-for-real-time-friends-rea-mw3vxzls | M2b desktop UI: egui login/2FA/main screens, friend sections + search, friends' worlds dashboard with lazy world-name resolution (`GET /worlds/{id}`, 10-min failure cache) | — |
| M3 Local data | in review | m3-coder | agent/m3-local-data-sqlite-vrchat-log-parser-t-mw3zy1r8 | vrc-logs log parser/tailer, vrc-store SQLite repos (timeline, friend history, sessions, stats), vrc-core commands + store thread, egui data tabs, cli timeline/stats/tail-logs | — |
| M4 Social actions | in review | m4-coder | agent/m4-social-actions-read-rules-md-first-ma-mw4606ou | vrc-api social endpoints, typed pipeline notification events, vrc-core commands + live notification state + throttled multi-invite, egui notifications/profile/invite UI, cli notifications/invite/profile/note | — |
| M5 Worlds, groups, avatars | in review | m5-coder | agent/m5-worlds-groups-avatars-read-rules-md-f-mw6erppm | vrc-api worlds/groups/avatars/favorites/instances endpoints + validators, vrc-core browse commands with debounced deduped search and detail caches, egui Worlds/Groups/Avatars/Favorites tabs, cli subcommands | — |
| M6 Media | in review | m6-coder | agent/m6-media-read-rules-md-first-mandatory-t-mw6jf0b1 | vrc-store media+settings repos, vrc-api byte/external POST paths, vrc-core scanner/PNG metadata/thumbnail LRU/Discord webhook relay, egui Media tab + card thumbnails, cli media | — |
| M7 Launcher and tools | in review | m7-coder | agent/m7-launcher-and-tools-read-rules-md-firs-mw6ndnxv | vrc-launcher Steam VDF/ACF detection + spawn-only launch (no-vr/profile/fps/OSC/midi/extra args, `vrchat://launch?id=` world join, companions), vrc-osc UDP client+listener with chatbox (144-char clamp, 1.5 s relay) and `/avatar/parameters/` tools, vrc-api documented `/calendar*` endpoints (month/discover/featured/search/group/next/follow), vrc-core tools commands + persisted companion/launch/OSC settings, egui Tools+Calendar tabs, cli launch/companions/chatbox/osc/calendar | — |
| M8 Optional | not started | | | | |

## Notes
- Orchestration tooling lives outside this repo.
