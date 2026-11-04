#!/usr/bin/env node
// MCP server wrapping the agent orchestrator — exposes the fleet as tools so
// any MCP client (Devin sessions included) can dispatch work without shelling
// out. Long runs always spawn detached (stdio calls can't block for 30 min).
//
// Tools:
//   orchestrate        — run tasks through manager→coder⇄reviewer→PR
//   orchestrate_resume — finish runs orphaned by a dead orchestrator
//   run_status         — stage/commits of every tracked worktree run
//   list_sessions      — devin CLI sessions in this repo + worktrees
//   tail_session       — recent transcript lines of a session (locked OK)
//   prune_sessions     — delete dead sessions (deleted working_directory)
//   prune_history      — trim .worktrees/.history breadcrumbs (age + count)
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { JSONRPCMessageSchema } from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import { execFileSync, spawn } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, readdirSync, readFileSync, realpathSync, statSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { PassThrough } from "node:stream";
import { StringDecoder } from "node:string_decoder";

const here = dirname(fileURLToPath(import.meta.url));
// --git-common-dir resolves to the MAIN worktree's .git even when launched
// from inside a linked worktree (where --show-toplevel would give the
// worktree root and scope wtRoot to a nested, wrong .worktrees).
let repoRoot;
try {
  const gitCommon = execFileSync("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    { cwd: here, encoding: "utf8", windowsHide: true }).trim();
  repoRoot = dirname(gitCommon);
} catch {
  console.error("mcp-server: not inside a git worktree");
  process.exit(1);
}
const wtRoot = join(repoRoot, ".worktrees");
const MAX_WORKTREE_SCAN = 200; // bound on .worktrees dirs scanned per call

const ok = (data) => ({ content: [{ type: "text", text: typeof data === "string" ? data : JSON.stringify(data, null, 2) }] });
const fail = (msg) => ({ content: [{ type: "text", text: msg }], isError: true });
// Strip control chars (except \t \n) — C0, DEL, C1 (\x80-\x9f; \u009b is
// CSI on ConPTY/Windows Terminal), U+2028/U+2029 (line/paragraph separators
// that break line-oriented client parsing) and all bidi controls — from
// anything echoed into tool results. .orch-state.json fields and session
// titles/content are agent-writable, so escape sequences must not reach
// the client's renderer. Non-strings are coerced; null/undefined pass through.
const scrub = (v) => v == null ? v : String(v).replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u2028-\u202e\u2066-\u2069]/g, "");
// Precise containment: ".." alone or "../x" escapes, but an entry literally
// named "..foo" is a valid in-repo name and must not be rejected.
const escapesRoot = (r) => !r || r === ".." || r.startsWith(".." + sep) || isAbsolute(r);

// Spawn orchestrate.mjs detached so the MCP call returns immediately and the
// run survives this server process exiting. Same recipe as --detach, except
// this server is long-lived: the log fd must be closed after spawn and every
// run gets its own log file (counter breaks same-millisecond ties).
let runCounter = 0;
function launchDetached(args) {
  const logDir = join(here, "runs");
  mkdirSync(logDir, { recursive: true });
  const log = join(logDir, `orchestrator-${Date.now().toString(36)}-${++runCounter}.log`);
  const fd = openSync(log, "a");
  let child;
  try {
    child = spawn(process.execPath, [join(here, "orchestrate.mjs"), ...args], {
      detached: true, stdio: ["ignore", fd, fd], cwd: repoRoot,
      env: process.env, windowsHide: true,
    });
  } catch (e) {
    closeSync(fd); // spawn threw synchronously — our fd copy was never duped
    throw e;
  }
  child.on("error", () => {}); // spawn failure → error event, not a crash
  child.unref();
  closeSync(fd); // the child's copy is already duped; don't leak ours
  if (child.pid === undefined) throw new Error("spawn failed: no pid assigned");
  return { pid: child.pid, log };
}

const server = new McpServer({ name: "kodex-orchestrator", version: "1.0.0" });

server.tool(
  "orchestrate",
  "Run task(s) through the manager→coder⇄reviewer→PR pipeline in isolated git worktrees. Returns immediately (detached run); poll run_status and tail the log.",
  {
    // Bounded: each element spawns a detached child, so an uncapped array is
    // a fork bomb and unbounded strings bloat every child's argv.
    tasks: z.array(z.string().trim().min(1).max(4000)).min(1).max(32)
      .optional().describe("Task descriptions (max 32, ≤4000 chars each). Ignored if tasks_file is set."),
    tasks_file: z.string().trim().min(1).max(1024).optional()
      .describe("Path to a tasks.md with '- [ ]' lines. Must resolve inside the repository."),
    parallel: z.boolean().optional().default(true).describe("Run tasks concurrently (default true)"),
  },
  async ({ tasks, tasks_file, parallel }) => {
    const args = [];
    if (tasks_file) {
      // Confine to the repo: absolute paths and ../ would turn the existence
      // check into an arbitrary-file oracle and hand any file to --tasks.
      const f = resolve(repoRoot, tasks_file);
      if (escapesRoot(relative(repoRoot, f)))
        return fail(`tasks_file must resolve inside the repository: ${scrub(tasks_file)}`);
      // Lexical containment isn't enough — a symlink/junction inside the
      // repo can point outside it, so compare canonical real paths too.
      let realF, realRel;
      try {
        realF = realpathSync(f);
        realRel = relative(realpathSync(repoRoot), realF);
        if (!statSync(realF).isFile()) throw new Error("not a file");
      } catch {
        return fail(`tasks file not found: ${scrub(tasks_file)}`);
      }
      if (escapesRoot(realRel))
        return fail(`tasks_file must resolve inside the repository: ${scrub(tasks_file)}`);
      args.push("--tasks", realF);
    } else if (tasks?.length) {
      // Trim before validating — "  --resume" must not reach argv either.
      const cleaned = tasks.map((t) => t.trim());
      const bad = cleaned.filter((t) => !t || t.startsWith("-"));
      if (bad.length) return fail(`task strings must be non-empty and not start with '-': ${scrub(bad.join(", "))}`);
      // One detached child per task — keeps each worktree's state/branch
      // exactly as a single-task run would produce. Inherently concurrent;
      // the parallel flag is a no-op here.
      const runs = [];
      const errors = [];
      for (const t of cleaned) {
        try { runs.push(launchDetached([t])); }
        catch (e) { errors.push(`"${scrub(t).slice(0, 60)}": ${e instanceof Error ? e.message : String(e)}`); }
      }
      // Report both halves: which tasks failed AND which pids/logs survived,
      // so the caller can track the runs that did launch.
      if (errors.length)
        return fail(`spawn failed: ${errors.join("; ")}` +
          (runs.length ? ` — ${runs.length} task(s) still launched: ${runs.map((r) => `pid ${r.pid} (log ${r.log})`).join(", ")}` : ""));
      return ok({ detached: runs.map((r) => ({ pid: r.pid, log: r.log })) });
    } else {
      return fail("provide tasks[] or tasks_file");
    }
    if (parallel) args.push("--parallel");
    try {
      const r = launchDetached(args);
      return ok({ pid: r.pid, log: r.log });
    } catch (e) {
      return fail(`spawn failed: ${e instanceof Error ? e.message : String(e)}`);
    }
  }
);

server.tool(
  "orchestrate_resume",
  "Finish orchestrator runs orphaned by a dead/killed process: re-plans, resumes coder sessions, reviews, pushes and opens PRs for unfinished .worktrees with .orch-state.json.",
  {},
  async () => {
    try {
      return ok(launchDetached(["--resume"]));
    } catch (e) {
      return fail(`spawn failed: ${e instanceof Error ? e.message : String(e)}`);
    }
  }
);

// Prune is quick but not instant (~100 sessions × one `devin rm` each, 6 at
// a time), so run the CLI as a child and wait rather than detaching.
function runOrchestratorSync(args, timeoutMs = 120_000) {
  return new Promise((resolve, reject) => {
    const p = spawn(process.execPath, [join(here, "orchestrate.mjs"), ...args],
      { cwd: repoRoot, timeout: timeoutMs, windowsHide: true, env: process.env });
    p.stdout.setEncoding("utf8");
    p.stderr.setEncoding("utf8");
    let out = "", err = "";
    p.stdout.on("data", (d) => { if (out.length < 1 << 20) out += d; });
    p.stderr.on("data", (d) => { if (err.length < 64 << 10) err += d; });
    p.on("error", reject);
    // Non-zero exits carry the stdout tail too — prune reports per-id
    // failures there, not on stderr.
    p.on("close", (code, signal) => code === 0
      ? resolve(err.trim() ? `${out}\n[stderr]\n${err.slice(-2000)}` : out)
      : reject(new Error(`orchestrate exited ${code ?? signal}: ${(err || out).slice(-1500)}`)));
  });
}

server.tool(
  "prune_sessions",
  "Delete devin CLI sessions that are pure history: working_directory no longer exists (finished orchestrator runs leave 3–5 agents each). Optionally also sessions under this repo idle more than `days`. The dead-directory rule applies to every session in the Devin database. dry_run (default true) lists targets without deleting.",
  {
    days: z.number().int().min(0).max(3650).optional().default(0)
      .describe("Also remove sessions under this repo idle more than this many days (default 0 = off)"),
    dry_run: z.boolean().optional().default(true)
      .describe("List what would be removed without deleting (default true)"),
  },
  async ({ days, dry_run }) => {
    const args = ["--prune-sessions"];
    if (days) args.push("--days", String(days));
    if (dry_run) args.push("--dry-run");
    try {
      // A worst-case prune is ~100 `devin rm` calls — well past the 120s default.
      return ok(await runOrchestratorSync(args, 600_000));
    } catch (e) {
      return fail(`prune failed: ${scrub(e instanceof Error ? e.message : String(e))}`);
    }
  }
);

server.tool(
  "prune_history",
  "Delete stale .worktrees/.history/*.json breadcrumbs left by finished runs: files older than `days` (default 30) plus any beyond the newest `keep` (default 100) — both rules apply. Local file deletes only; sessions are untouched. dry_run (default true) lists targets without deleting.",
  {
    days: z.number().int().min(0).max(3650).optional().default(30)
      .describe("Remove history files older than this many days (default 30; 0 = age check off)"),
    keep: z.number().int().min(0).max(10000).optional().default(100)
      .describe("Keep at most this many newest history files (default 100)"),
    dry_run: z.boolean().optional().default(true)
      .describe("List what would be removed without deleting (default true)"),
  },
  async ({ days, keep, dry_run }) => {
    // days/keep are always passed: omitting them would fall back to the
    // CLI's own defaults, and an explicit days=0 turns the age check off.
    const args = ["--prune-history", "--days", String(days), "--keep", String(keep)];
    if (dry_run) args.push("--dry-run");
    try {
      // Local file deletes only — the default 120 s timeout is plenty.
      return ok(await runOrchestratorSync(args));
    } catch (e) {
      return fail(`prune failed: ${scrub(e instanceof Error ? e.message : String(e))}`);
    }
  }
);

server.tool(
  "run_status",
  "Report every tracked orchestrator run: stage, task, branch, commits ahead of main.",
  {},
  async () => {
    const runs = [];
    let scannedAll = true;
    if (existsSync(wtRoot)) {
      // Bound the scan — each dir costs a JSON parse plus a blocking git
      // child, so a flooded .worktrees dir must not stall the event loop.
      // Residual ceiling: up to MAX_WORKTREE_SCAN synchronous spawns; the
      // deadline caps the block at ~20 s (15 s + one in-flight git call).
      const dirs = readdirSync(wtRoot, { withFileTypes: true })
        .filter((e) => e.isDirectory() && !e.name.startsWith(".")); // .history is bookkeeping
      const deadline = Date.now() + 15_000;
      scannedAll = dirs.length <= MAX_WORKTREE_SCAN;
      for (const e of dirs.slice(0, MAX_WORKTREE_SCAN)) {
        if (Date.now() > deadline) { scannedAll = false; break; }
        const wt = join(wtRoot, e.name);
        // A state file is a few hundred bytes — size-check before the
        // read+parse so a giant agent-written file can't block/OOM us.
        let state = null;
        try {
          const sf = join(wt, ".orch-state.json");
          if (statSync(sf).size <= 1024 * 1024) state = JSON.parse(readFileSync(sf, "utf8"));
        } catch {}
        let commits = "?";
        try { commits = execFileSync("git", ["rev-list", "--count", "main..HEAD"], { cwd: wt, encoding: "utf8", timeout: 5000, windowsHide: true }).trim(); } catch {}
        runs.push({
          worktree: scrub(e.name),
          stage: scrub(state?.stage) ?? "pre-state (manual salvage)",
          task: scrub(state?.task)?.slice(0, 80),
          branch: scrub(state?.branch),
          coder: scrub(state?.coderId),
          commits_ahead: commits,
        });
      }
    }
    if (!runs.length) return ok("no worktree runs");
    // runs.length is the real scanned count — the deadline break may cut it short.
    return ok(scannedAll ? runs : { scanned: runs.length, truncated: true, runs });
  }
);

// Sessions + transcripts live in the CLI's own sessions.db (SQLite WAL —
// read-only readers never block a running agent). Same source as tail.mjs.
// Lazy singleton: one connection for the server's lifetime, and lazy
// node:sqlite import so Node <22.13 only fails on the two db-backed tools.
// The in-flight promise is cached, not just the resolved db — otherwise
// concurrent first calls double-open sessions.db and leak a connection.
let dbPromise;
async function openSessionsDb() {
  // Same lookup as orchestrate.mjs --prune-sessions.
  const p = process.env.DEVIN_SESSIONS_DB || join(process.env.APPDATA ?? "", "devin", "cli", "sessions.db");
  if (!(process.env.DEVIN_SESSIONS_DB || process.env.APPDATA) || !existsSync(p))
    throw new Error(`devin sessions.db not found at ${p}`);
  const { DatabaseSync } = await import("node:sqlite");
  return new DatabaseSync(p, { readOnly: true });
}
function sessionsDb() {
  dbPromise ??= openSessionsDb();
  // A failed open clears the cache so a later call retries once the db exists.
  return dbPromise.catch((e) => { dbPromise = undefined; throw e; });
}

server.tool(
  "list_sessions",
  "List devin CLI sessions whose working_directory is this repo or one of its .worktrees — i.e. the agents this orchestrator spawned plus your own.",
  {},
  async () => {
    const db = await sessionsDb();
    const wtDirs = existsSync(wtRoot)
      ? readdirSync(wtRoot, { withFileTypes: true }).filter((e) => e.isDirectory() && !e.name.startsWith("."))
      : [];
    const dirs = [repoRoot, ...wtDirs.slice(0, MAX_WORKTREE_SCAN).map((e) => join(wtRoot, e.name))]
      .map((d) => resolve(d).toLowerCase());
    const rows = db.prepare(
      "SELECT id, title, working_directory, last_activity_at FROM sessions ORDER BY last_activity_at DESC LIMIT 50"
    ).all();
    // Require an absolute path — resolve("") would map to this server's
    // cwd and false-positive unrelated sessions into the listing.
    const mine = rows.filter((s) =>
      typeof s.working_directory === "string" && isAbsolute(s.working_directory) &&
      dirs.includes(resolve(s.working_directory).toLowerCase()));
    const sessions = mine.map((s) => ({
      id: scrub(s.id),
      title: scrub(s.title),
      cwd: scrub(s.working_directory),
      active: Date.now() / 1000 - s.last_activity_at < 90,
      last_activity_at: s.last_activity_at,
    }));
    // Same truncation marker as run_status — don't silently drop worktrees.
    return ok(wtDirs.length <= MAX_WORKTREE_SCAN ? sessions : { scanned: MAX_WORKTREE_SCAN, truncated: true, sessions });
  }
);

server.tool(
  "tail_session",
  "Dump the most recent transcript lines of a devin session — works on locked sessions (`devin -r` refuses double-attach).",
  {
    id: z.string().min(1).max(128).describe("Session id or unique prefix"),
    limit: z.number().int().min(1).max(500).optional().default(30).describe("Max messages to return (default 30)"),
  },
  async ({ id, limit }) => {
    const db = await sessionsDb();
    // Escape LIKE wildcards AND the escape char itself so caller-supplied
    // %, _ or \ can't widen or reshape the match.
    const escaped = id.replace(/[\\%_]/g, (c) => "\\" + c);
    const session = db.prepare(
      "SELECT id, title FROM sessions WHERE id LIKE ? ESCAPE '\\' ORDER BY last_activity_at DESC"
    ).get(`${escaped}%`);
    if (!session) return fail(`no session matching '${scrub(id)}'`);
    const rows = db.prepare(
      "SELECT chat_message FROM message_nodes WHERE session_id=? ORDER BY row_id DESC LIMIT ?"
    ).all(session.id, limit);
    const lines = [];
    const seen = new Set(); // streaming updates re-send one message under new row_ids
    for (const r of rows.reverse()) {
      // Bound the blob before parse: row count is capped but blob size is
      // not — one multi-MB chat_message must not stall the event loop.
      if (typeof r.chat_message !== "string" || r.chat_message.length > 256 * 1024) continue;
      let m; try { m = JSON.parse(r.chat_message); } catch { continue; }
      if (m.message_id && seen.has(m.message_id)) continue;
      if (m.message_id) seen.add(m.message_id);
      if (m.role === "assistant") {
        const t = scrub(m.content ?? "").trim();
        if (t) lines.push("[agent] " + t.slice(0, 500));
        // tool_calls is model/db-written — could be a non-array, hold
        // nulls, or run to thousands; a malformed blob must not fail or
        // balloon the whole tail.
        const calls = Array.isArray(m.tool_calls) ? m.tool_calls : [];
        let shown = 0;
        for (const c of calls) {
          if (!c || typeof c !== "object") continue;
          if (++shown > 25) { lines.push(`  -> … ${calls.length - 25} more tool_calls`); break; }
          lines.push(`  -> ${scrub(c.name)} ${scrub(JSON.stringify(c.arguments ?? {})).slice(0, 80)}`);
        }
      } else if (m.role === "tool") {
        const c = scrub(m.content ?? "").replace(/\s+/g, " ").trim();
        if (c) lines.push("[tool ] " + c.slice(0, 200));
      } else if (m.role === "user") {
        lines.push("[user ] " + scrub(m.content ?? "").slice(0, 200));
      }
    }
    return ok(`=== ${scrub(session.id)} — ${scrub(session.title)}\n\n` + lines.join("\n"));
  }
);

// ---------- stdin framing guard ----------
// Frame stdin into lines before the SDK's ReadBuffer sees them: its 10 MiB
// cap closes the whole transport on overflow, and malformed lines are
// swallowed via onerror with no reply. Here oversized lines, non-JSON text
// and batch arrays each get a JSON-RPC error on stdout, only schema-valid
// single messages are forwarded — and the server stays alive throughout.
// .length counts UTF-16 code units, so this caps ~1M chars (up to ~4 MiB
// on the wire) — bounded and still under the SDK's 10 MiB ReadBuffer cap.
const MAX_LINE_CHARS = 1024 * 1024;
const framedStdin = new PassThrough();
framedStdin.on("error", () => {}); // stdin errors before connect() must not be unhandled

function rpcError(id, code, message) {
  // id: null per JSON-RPC — these errors precede any parsed request id.
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, error: { code, message } }) + "\n");
}

function forwardLine(line) {
  let msg;
  // Blank lines get a -32700 too — strict per spec; a client sending
  // "\n" keepalives sees one error each, never a crash or a hang.
  try { msg = JSON.parse(line); }
  catch { return rpcError(null, -32700, "Parse error"); }
  if (Array.isArray(msg)) return rpcError(null, -32600, "Invalid Request: batch calls are not supported");
  const id = typeof msg?.id === "string" || Number.isInteger(msg?.id) ? msg.id : null;
  if (!JSONRPCMessageSchema.safeParse(msg).success) return rpcError(id, -32600, "Invalid Request");
  framedStdin.write(line + "\n");
}

{
  const decoder = new StringDecoder("utf8");
  let pending = "";
  let dropping = false; // discarding the rest of an oversized line
  process.stdin.on("data", (chunk) => {
    let text = decoder.write(chunk);
    while (text.length) {
      if (dropping) {
        const nl = text.indexOf("\n");
        if (nl === -1) return;
        dropping = false;
        text = text.slice(nl + 1);
        continue;
      }
      const nl = text.indexOf("\n");
      if (nl === -1) {
        pending += text;
        if (pending.length > MAX_LINE_CHARS) {
          pending = "";
          dropping = true;
          rpcError(null, -32600, `Invalid Request: line exceeds ${MAX_LINE_CHARS} characters`);
        }
        return;
      }
      const line = pending + text.slice(0, nl);
      pending = "";
      if (line.length > MAX_LINE_CHARS) rpcError(null, -32600, `Invalid Request: line exceeds ${MAX_LINE_CHARS} characters`);
      else forwardLine(line.replace(/\r$/, ""));
      text = text.slice(nl + 1);
    }
  });
  process.stdin.on("end", () => framedStdin.end());
  process.stdin.on("error", (e) => framedStdin.destroy(e));
}

await server.connect(new StdioServerTransport(framedStdin));
