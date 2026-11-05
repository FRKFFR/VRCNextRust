# STATUS

| Milestone | State | Session | Branch / PR | Outcome | Open issues |
|---|---|---|---|---|---|
| Setup | done | lead | main | repo, RULES.md, architecture, orchestrator | — |
| M1 Foundations | M1b in review | m1b-coder | agent/m1b-vrc-api-client-read-rules-md-first-m-mw3qpgur | M1b vrc-api client: auth + 2FA, CookieVault (keyring/memory), token-bucket rate limiter + retry, models, wiremock tests | clipboard-win 5.4.3 via eframe accepted as pin exception; re-pin to 5.4.2 after 2026-11-06 |
| M2 Real-time | M2a in review | m2a-coder | agent/m2a-real-time-data-layer-read-rules-md-f-mw3so8op | real-time data layer: paginated friends/favorites in vrc-api, reconnecting WS pipeline (vrc-pipeline), vrc-core App merging REST+events into AppState (sections/search/worlds), vrcnext-cli login/me/friends/watch/logout | — |
| M3 Local data | not started | | | | agent time limit 60 min |
| M4 Social actions | not started | | | | |
| M5 Worlds, groups, avatars | not started | | | | |
| M6 Media | not started | | | | |
| M7 Launcher and tools | not started | | | | |
| M8 Optional | not started | | | | |

## Notes
- Orchestration tooling lives outside this repo.
