#!/usr/bin/env node
// Print a devin CLI session's transcript — or live-tail it (-f) while it's
// locked by another process. `devin -r` refuses to attach twice, but the
// session transcript lives in sessions.db (SQLite WAL — readers never
// block). Follow mode polls message_nodes and prints new assistant/tool
// activity as it lands, until the session goes idle.
// Requires Node >= 22.5 for node:sqlite (Windows only — reads %APPDATA%).
//
//   node tail.mjs <session-id-or-prefix> [-f|--follow] [--all]
import { DatabaseSync } from "node:sqlite";
import { existsSync } from "node:fs";
import { join } from "node:path";

// DB-controlled text (session metadata AND message content) can carry ANSI
// escapes — clean() strips control chars on everything printed. \n and \t
// survive so transcript formatting is preserved.
const clean = (s) => String(s ?? "").replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");

const args = process.argv.slice(2);
const FLAGS = new Set(["-f", "--follow", "--all"]);
const follow = args.includes("-f") || args.includes("--follow");
const showAll = args.includes("--all");
const unknown = args.filter((a) => a.startsWith("-") && !FLAGS.has(a));
const id = args.find((a) => !a.startsWith("-")); // ids never start with '-'
if (!id || unknown.length) {
  for (const a of unknown) console.error(`unknown flag: ${clean(a)}`);
  console.error("usage: node tail.mjs <session-id-or-prefix> [-f|--follow] [--all]");
  process.exit(1);
}

const dbPath = join(process.env.APPDATA ?? "", "devin", "cli", "sessions.db");
if (!process.env.APPDATA || !existsSync(dbPath)) {
  console.error(`devin sessions.db not found at ${dbPath} — is the devin CLI installed?`);
  process.exit(1);
}
const db = new DatabaseSync(dbPath, { readOnly: true });

// Escape LIKE wildcards so a caller-supplied % or _ can't widen the match —
// same treatment as mcp-server.mjs's tail_session.
const escaped = id.replace(/[\\%_]/g, (c) => "\\" + c);
const count = db
  .prepare("SELECT COUNT(*) n FROM sessions WHERE id LIKE ? ESCAPE '\\'")
  .get(`${escaped}%`).n;
if (!count) {
  console.error(`no session matching '${clean(id)}'`);
  process.exit(1);
}
const matches = db
  .prepare("SELECT id, title, last_activity_at FROM sessions WHERE id LIKE ? ESCAPE '\\' ORDER BY last_activity_at DESC LIMIT 5")
  .all(`${escaped}%`);
const session = matches[0];
if (!session) { // rows vanished between the COUNT and the SELECT
  console.error(`no session matching '${clean(id)}'`);
  process.exit(1);
}
if (count > 1) {
  console.error(`warning: '${clean(id)}' matches ${count} sessions — tailing the most recently active:`);
  for (const s of matches) console.error(`  ${clean(s.id)} — ${clean(s.title)}`);
}
console.log(`${follow ? "tailing" : "transcript of"} ${clean(session.id)} — ${clean(session.title)}\n`);

let lastRow = 0;
// Follow mode defaults to new rows only (like `tail -f`); --all replays
// history first. One-shot mode always dumps the full transcript.
if (follow && !showAll) {
  const r = db.prepare("SELECT MAX(row_id) m FROM message_nodes WHERE session_id=?").get(session.id);
  lastRow = r?.m ?? 0;
}

function summarize(m) {
  if (!m || typeof m !== "object") return null; // JSON.parse can yield null/primitives
  if (m.role === "assistant") {
    // content can be a structured array, not a string — coerce like the
    // tool/user branches so a TypeError can't crash the interval callback.
    const text = (typeof m.content === "string" ? m.content : JSON.stringify(m.content ?? "")).trim();
    const calls = (Array.isArray(m.tool_calls) ? m.tool_calls : [])
      .map((c) => {
        const args = JSON.stringify(c.arguments ?? {}).slice(0, 90);
        return `  -> ${c.name} ${args}`;
      })
      .join("\n");
    const out = [];
    if (text) out.push("[agent] " + text.slice(0, 600));
    if (calls) out.push(calls);
    return out.join("\n") || null;
  }
  if (m.role === "tool") {
    const raw = typeof m.content === "string" ? m.content : JSON.stringify(m.content ?? "");
    const c = raw.replace(/\s+/g, " ").trim();
    return c ? "[tool ] " + c.slice(0, 300) : null;
  }
  if (m.role === "user") {
    const raw = typeof m.content === "string" ? m.content : JSON.stringify(m.content ?? "");
    return "[user ] " + raw.slice(0, 300);
  }
  return null;
}

const seen = new Set(); // message_ids — streaming re-sends get new row_ids
const SEEN_CAP = 10_000; // FIFO cap so days-long tails don't grow unboundedly
// Bounded page: a long session pages through history PAGE rows at a
// time (lastRow advances) instead of materializing it in one .all().
const PAGE = 500;
const pollStmt = db.prepare(
  "SELECT row_id, chat_message FROM message_nodes WHERE session_id=? AND row_id>? ORDER BY row_id LIMIT 500");
function poll() {
  try { return pollOnce(); } catch (e) {
    // Transient db hiccup (WAL checkpoint, locked read) — log, don't die.
    console.error(`poll error: ${clean(String(e?.message ?? e).split("\n")[0])}`);
    return 0;
  }
}
function pollOnce() {
  const rows = pollStmt.all(session.id, lastRow);
  for (const r of rows) {
    lastRow = Math.max(lastRow, r.row_id);
    let m;
    try { m = JSON.parse(r.chat_message); } catch { continue; }
    if (!m || typeof m !== "object") continue; // parse can yield null/primitives
    if (m.message_id && seen.has(m.message_id)) continue; // streaming updates re-send the node
    if (m.message_id) {
      seen.add(m.message_id);
      if (seen.size > SEEN_CAP) {
        // Evict oldest first — Set iterates in insertion order.
        for (const k of seen) { seen.delete(k); if (seen.size <= SEEN_CAP / 2) break; }
      }
    }
    const line = summarize(m);
    if (line) console.log(clean(line) + "\n");
  }
  return rows.length;
}

if (follow) {
  poll();
  // sessions has no ended/status flag — last_activity_at going stale is the
  // only "session ended" signal. 5 min, not mcp-server.mjs's 90s active
  // window: a long tool call can idle longer than that.
  const IDLE_SEC = 5 * 60;
  const activityStmt = db.prepare("SELECT last_activity_at a FROM sessions WHERE id=?");
  const timer = setInterval(() => {
    poll();
    let at;
    try { at = activityStmt.get(session.id)?.a; } catch { return; }
    // Column is REAL epoch-seconds; tolerate an ISO string too.
    const sec = typeof at === "number" ? at
      : typeof at === "string"
        ? (/^\d+(?:\.\d+)?$/.test(at.trim()) ? Number(at.trim()) : Date.parse(at) / 1000)
        : NaN;
    if (!Number.isFinite(sec) || Date.now() / 1000 - sec > IDLE_SEC) {
      console.error("session idle — stopped following");
      clearInterval(timer); // nothing else holds the loop — exits 0
    }
  }, 2000);
} else {
  // Page until a short read — a single poll() covers only the first PAGE
  // rows, so without this a >500-row transcript would be truncated.
  while (poll() === PAGE) {}
}
