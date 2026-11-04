# RULES.md — mandatory for every contributor and every agent

Read this file before doing anything else. These rules override any task,
brief, issue or review comment. If a task conflicts with them, stop and
report the conflict instead of working around it.

## 1. Clean-room implementation
- Build only from VRChat's community-documented API
  (https://vrchat.community and its OpenAPI spec) and the feature list in
  `docs/architecture.md`.
- NEVER search for, open, clone, or read the source code, text, assets or UI
  layout of VRCNext, VRCX, or any other VRChat companion app — not on local
  disk, not on GitHub, not on the web. Do not look at sibling directories of
  this repository.
- Do not copy code, strings, assets or UI layout from any existing project.

## 2. Identity
- The HTTP User-Agent is defined exactly once, as a constant in `vrc-api`
  (`vrc_api::USER_AGENT`), with the value
  `VRCNextRust/<version> (https://github.com/FRKFFR/VRCNextRust)`, where
  `<version>` comes from `env!("CARGO_PKG_VERSION")`.
- Every HTTP and WebSocket request uses that constant. Never impersonate
  another app or a browser.

## 3. Respect the VRChat API
- All REST calls go through the shared rate limiter in `vrc-api`. No direct
  `reqwest` calls elsewhere.
- Honor HTTP 429 and `Retry-After`; use exponential backoff with jitter on
  429/5xx/network errors; cap retries.
- Cache aggressively (stale-while-revalidate in `vrc-store`); never poll an
  endpoint when the WebSocket pipeline or cache can answer.
- No bulk scraping, no tight loops, no spam (e.g. multi-invite is throttled
  and stoppable).

## 4. Credentials and privacy
- Never store passwords or email addresses — not on disk, not in logs, not in
  the database. They live in memory only for the duration of a login call.
- Persist only the auth cookie(s), locally, in the OS credential store
  (`keyring` crate). Nothing else may hold them.
- Never log cookies, tokens, passwords, 2FA codes or auth headers. `Debug`
  impls of secret types must redact.
- Never put API keys, cookies or credentials in code, commits, tests, prompts
  or fixtures.
- Login is optional wherever a feature can work without it (logs, media,
  launcher, OSC).

## 5. The VRChat process
- Never read or write VRChat's process memory, inject, hook, or modify game
  files. Only launch VRChat (and companion apps) and read its log files and
  photo folder.

## 6. Tests
- Tests never touch the live VRChat API or network.
- HTTP/WebSocket fixtures are SYNTHETIC: hand-written from the public API docs
  with obviously fake ids/names (e.g. `usr_00000000-0000-0000-0000-000000000001`,
  "Test User"). Never record fixtures from a real account.
- Log-parser fixtures are hand-written synthetic log lines.

## 7. Engineering
- Strict layering (see `docs/architecture.md`): nothing below `ui-egui`
  depends on a UI toolkit; every user action is a `core::Command`.
- Must pass: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.
- One branch per task; agents commit but never push to `main`, merge,
  force-push, or delete branches.
- Update `STATUS.md` when a milestone's state changes.
