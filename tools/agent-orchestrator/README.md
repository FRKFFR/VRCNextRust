# Agent orchestrator

Runs a manager → coder ⇄ reviewer peer loop on the **Devin CLI**: a task goes
in, a pull request comes out. You review PRs, not conversations.

Each agent is a headless `devin -p` session in an isolated git worktree; the
coder's session is resumed (`devin -r`) for fix rounds so it keeps context.

Agents run with `--permission-mode dangerous` — **unsandboxed**, with your
full local permissions. The manager/reviewer "read-only" and coder "don't
push" limits are prompts, not enforcement. Don't point it at untrusted tasks.

## Setup

Auth: either a logged-in `devin` CLI session **or** a Devin personal access
token (devin.ai → Settings → API → create token). The token is the more
reliable path headlessly — put it in `tools/agent-orchestrator/.env`:

```
DEVIN_API_KEY=<your-pat>
```

(or export it, or use the repo-root `.env` — all gitignored). PR creation
reuses the github.com credential already in your credential manager. Needs
`devin` on PATH and Node ≥ 18. `npm install` is not required for the CLI
itself (zero deps) — only `mcp-server.mjs` needs it.

Optional: `DEVIN_MODEL` picks the model for every agent;
`ORCHESTRATOR_RUN_TIMEOUT_MS` caps each agent run (default 30 min).

## Usage

```bash
node orchestrate.mjs "add a --verbose flag to server-sync"
node orchestrate.mjs --tasks tasks.md          # one "- [ ] task" line each
node orchestrate.mjs --tasks tasks.md --parallel
node orchestrate.mjs --tasks tasks.md --parallel --detach   # survives shell death
node orchestrate.mjs --resume                  # finish runs orphaned by a crash
node orchestrate.mjs --prune-sessions          # rm sessions whose working_directory is gone
node orchestrate.mjs --prune-sessions --days 30 --dry-run   # preview idle>30d too
node orchestrate.mjs --prune-history           # rm .history crumbs >30d old, keep newest 100
node orchestrate.mjs --prune-history --days 7 --keep 50 --dry-run   # preview
node tail.mjs <session-id>                     # dump a session's transcript
node tail.mjs -f <session-id>                  # live-tail a locked session
node tail.mjs -f --all <session-id>            # live-tail, replaying history first
```

`--detach` respawns detached with output to `runs/*.log` so the run outlives
the launching shell. Every worktree carries `.orch-state.json`
(task/brief/coderId/stage); `--resume` re-enters unfinished runs at the right
stage — re-plans, resumes the coder (a partial commit is not a finished one),
re-reviews, and pushes + opens the PR on approval (reusing an existing PR if
a crashed attempt already opened one).

Runbook: [docs/orchestrator-runbook.md](../../docs/orchestrator-runbook.md)
covers the flags, run stages, and failure modes in depth.

`--prune-sessions` deletes Devin sessions through `devin rm --force`. Here is
what it matches:

- **Dead directory:** any session in the Devin database whose
  `working_directory` no longer exists, from any project. Those sessions
  can't be resumed into their worktree anymore. Permission or network-drive
  errors don't count as "gone".
- **`--days N`:** only sessions under this repo, worktrees included, that
  have been idle for more than N days. Idle sessions in other projects are
  left alone.

Run it with `--dry-run` first. The database is read from
`%APPDATA%\devin\cli\sessions.db`. On other hosts, set `DEVIN_SESSIONS_DB`.
Pruning needs Node ≥ 22.13.

## What happens per task

1. A `git worktree` is created under `.worktrees/<slug>` on branch
   `agent/<slug>` (ignored via `.gitignore`).
2. **Manager** (headless session) reads the repo and produces a brief:
   goal / files / acceptance criteria.
3. **Coder** (headless session) implements in the worktree, runs the repo's
   checks, commits on the branch. Never pushes — the script does that.
4. **Reviewer** (fresh headless session, told read-only) critiques
   `main...HEAD` and replies `VERDICT: APPROVED` or
   `VERDICT: CHANGES_REQUESTED` + findings.
5. Findings are sent verbatim back to the **coder's resumed session** — the
   peer loop — up to 3 rounds. On approval the script pushes the branch and
   opens the PR.

A task that fails all rounds keeps its worktree for inspection (cleanup
command is printed) and exits non-zero.

## Watching the agents

Each agent's output streams live, prefixed `[manager]` / `[coder]` /
`[reviewer]`. Spawned sessions also show up in `devin list`; open one
interactively with `devin -r <session-id>` (printed as
`[orch] coder session: <id>`) to scroll its full transcript. Sessions locked
by a running orchestrator can't be `-r`'d — use `node tail.mjs -f <id>` instead
(reads the CLI's sessions.db read-only; needs Node ≥ 22.5, Windows).

## MCP server

`mcp-server.mjs` exposes the fleet as MCP tools so any MCP client (Devin
sessions included) can dispatch and watch it without shell access:

- `orchestrate` — run tasks through the pipeline (detached; returns pid+log)
- `orchestrate_resume` — finish orphaned runs
- `run_status` — stage/commits of every tracked worktree
- `list_sessions` — devin sessions in this repo + worktrees
- `tail_session` — recent transcript lines of a session (locked OK)
- `prune_sessions` — delete dead sessions (dry_run=true by default)
- `prune_history` — trim `.worktrees/.history` breadcrumbs (dry_run=true by default)

Requires `npm install` here (adds `@modelcontextprotocol/sdk` + `zod`; the
CLI itself stays zero-dep). `list_sessions`/`tail_session` need Node ≥
22.13 (`node:sqlite`); the rest work on ≥18. Register in
`.devin/mcp_config.local.json`:

```json
"kodex-orchestrator": {
  "command": "node",
  "args": ["<abs path>/tools/agent-orchestrator/mcp-server.mjs"],
  "transport": "stdio"
}
```

`test-mcp.mjs` is a stdio smoke + adversarial test (initialize + tools/list
+ tool calls + hostile framing/input probes).
`test-prune.mjs` checks prune's selection logic against a temporary database
in dry-run mode, so it never deletes anything.
