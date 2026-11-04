// Smoke + adversarial test for mcp-server.mjs — speaks JSON-RPC over stdio.
// The seven tools must respond normally, and hostile framing/inputs must get
// clean errors while the server stays alive. `orchestrate` is only ever
// called with inputs rejected before spawn — this test never launches runs.
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const p = spawn(process.execPath, ["mcp-server.mjs"], { cwd: import.meta.dirname, stdio: ["pipe", "pipe", "pipe"] });
p.on("error", (e) => console.error("[spawn]", e));
// A dead server mid-test makes stdin writes EPIPE — swallow it and let the
// waitFor timeouts report the failure instead of crashing the harness.
p.stdin.on("error", () => {});
p.stderr.on("data", (d) => console.error("[stderr]", String(d).slice(0, 300)));

const responses = [];
const badLines = [];
let buf = "";
p.stdout.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) !== -1) {
    const line = buf.slice(0, i);
    buf = buf.slice(i + 1);
    if (!line.trim()) continue;
    try { responses.push(JSON.parse(line)); }
    catch { badLines.push(line.slice(0, 200)); }
  }
});

const send = (o) => p.stdin.write(typeof o === "string" ? o : JSON.stringify(o) + "\n");
// `from` skips responses received before the send, so identical-looking
// errors (id:null, -32600) from different probes can't satisfy each other.
const waitFor = (pred, from = 0, ms = 15000) => new Promise((resolve_, reject) => {
  const t0 = Date.now();
  const iv = setInterval(() => {
    const hit = responses.find((r, i) => i >= from && pred(r));
    if (hit) { clearInterval(iv); resolve_(hit); }
    else if (Date.now() - t0 > ms) { clearInterval(iv); reject(new Error("timeout waiting for response")); }
  }, 20);
});
const sendAndWait = (payload, pred) => { const from = responses.length; send(payload); return waitFor(pred, from); };

let nextId = 1;
const call = (method, params) => {
  const id = nextId++;
  return sendAndWait({ jsonrpc: "2.0", id, method, params }, (r) => r.id === id);
};
const callTool = (name, args) => call("tools/call", { name, arguments: args });

const results = [];
const check = (name, cond, detail = "") => {
  results.push({ name, ok: !!cond });
  console.log(`${cond ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
};
const isErr = (r) => r?.result?.isError === true;
const errText = (r) => r?.result?.content?.[0]?.text?.slice(0, 140);

try {
  const init = await call("initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "t", version: "1" } });
  check("initialize", init.result?.serverInfo?.name === "kodex-orchestrator");
  send({ jsonrpc: "2.0", method: "notifications/initialized", params: {} });

  const tools = await call("tools/list", {});
  const names = tools.result?.tools?.map((t) => t.name).sort().join(", ");
  check("tools/list exposes seven tools",
    names === "list_sessions, orchestrate, orchestrate_resume, prune_history, prune_sessions, run_status, tail_session", names);

  const pruneDry = await callTool("prune_sessions", { days: 0, dry_run: true });
  check("prune_sessions dry_run responds", !!pruneDry.result && !pruneDry.error, errText(pruneDry));
  // No db on this host → a clean isError; with one → the dry-run summary.
  check("prune_sessions dry_run reports targets, never deletes",
    isErr(pruneDry) ? /sessions\.db|Node/.test(errText(pruneDry))
      : /would be removed/.test(pruneDry.result?.content?.[0]?.text ?? "") &&
        !/ removed, /.test(pruneDry.result?.content?.[0]?.text ?? ""),
    errText(pruneDry));

  const pruneBad = await callTool("prune_sessions", { days: 100_000 });
  check("prune_sessions out-of-range days rejected", isErr(pruneBad) || !!pruneBad.error, errText(pruneBad));

  // No sessions.db needed: prune_history is pure local file deletes, and a
  // missing .history dir is a no-op success — never an isError.
  const phDry = await callTool("prune_history", { dry_run: true });
  check("prune_history dry_run responds, never deletes",
    !!phDry.result && !phDry.result.isError &&
      /would be removed|nothing to prune/.test(phDry.result?.content?.[0]?.text ?? ""),
    errText(phDry));

  const phBad = await callTool("prune_history", { keep: -1 });
  check("prune_history out-of-range keep rejected", isErr(phBad) || !!phBad.error, errText(phBad));

  const status = await callTool("run_status", {});
  check("run_status responds", !!status.result && !status.error);

  const sessions = await callTool("list_sessions", {});
  check("list_sessions responds", !!sessions.result && !sessions.error, errText(sessions));
  // A real session id (when sessions.db exists) enables positive checks:
  // tail by exact id works, and LIKE wildcards provably don't widen.
  let realId;
  try { realId = JSON.parse(sessions.result?.content?.[0]?.text ?? "[]")?.[0]?.id; } catch {}

  // --- hostile framing ---
  const malformed = await sendAndWait("{ not json at all\n",
    (r) => r.id === null && r.error?.code === -32700);
  check("malformed JSON → -32700 parse error", !!malformed);

  const batch = await sendAndWait(
    JSON.stringify([{ jsonrpc: "2.0", id: 77, method: "tools/list", params: {} },
      { jsonrpc: "2.0", method: "notifications/initialized", params: {} }]) + "\n",
    (r) => r.id === null && r.error?.code === -32600 && /batch/.test(r.error.message));
  check("batch array → -32600", !!batch);

  const oversized = await sendAndWait(
    '{"jsonrpc":"2.0","id":88,"method":"tools/list","padding":"' + "A".repeat(2 * 1024 * 1024) + '"}\n',
    (r) => r.id === null && r.error?.code === -32600 && /exceeds/.test(r.error.message));
  check("oversized line → -32600", !!oversized);

  const invalid = await sendAndWait('{"jsonrpc":"2.0","method":42}\n',
    (r) => r.error?.code === -32600);
  check("schema-invalid frame → -32600", !!invalid);

  // --- hostile tool inputs ---
  const dash = await callTool("orchestrate", { tasks: ["--resume"] });
  check("'-'-leading task rejected", isErr(dash), errText(dash));

  const padded = await callTool("orchestrate", { tasks: ["   --parallel  "] });
  check("padded '-'-leading task rejected", isErr(padded), errText(padded));

  const empty = await callTool("orchestrate", { tasks: ["   "] });
  check("whitespace-only task rejected", isErr(empty), errText(empty));

  const many = await callTool("orchestrate", { tasks: Array(10_000).fill("do x") });
  check("10k-element tasks array rejected", isErr(many), errText(many));

  const escape = await callTool("orchestrate", { tasks_file: "../../../outside.md" });
  check("../ tasks_file rejected", isErr(escape) && /inside the repository/.test(errText(escape)), errText(escape));

  const absolute = await callTool("orchestrate", { tasks_file: join(tmpdir(), "not-in-repo.md") });
  check("absolute tasks_file outside repo rejected", isErr(absolute), errText(absolute));

  const missing = await callTool("orchestrate", { tasks_file: "no-such-file-inside-repo.md" });
  check("missing in-repo tasks_file → clean error", isErr(missing) && /not found/.test(errText(missing)), errText(missing));

  const dotdot = await callTool("orchestrate", { tasks_file: "..foo.md" });
  check("'..foo' in-repo name reaches existence check", isErr(dotdot) && /not found/.test(errText(dotdot)), errText(dotdot));

  const wild = await callTool("tail_session", { id: "%_" });
  check("'%_' session id → bounded error, no crash", isErr(wild), errText(wild));

  const backslash = await callTool("tail_session", { id: "\\" });
  check("'\\' session id → bounded error, no crash", isErr(backslash), errText(backslash));

  if (realId) {
    const tail = await callTool("tail_session", { id: realId, limit: 5 });
    check("tail_session on real session id", !!tail.result && !tail.result.isError, errText(tail));
    // Only meaningful against a populated db: bare '%' must not match it.
    const pct = await callTool("tail_session", { id: "%" });
    check("'%' wildcard does not match real sessions",
      isErr(pct) && /no session matching/.test(errText(pct)), errText(pct));
  }

  const longId = await callTool("tail_session", { id: "x".repeat(500) });
  check("oversized session id rejected", isErr(longId), errText(longId));

  // --- liveness after all of the above ---
  const alive = await callTool("run_status", {});
  check("server still alive after hostile input", !!alive.result && !alive.error);

  check("no unparseable stdout lines", badLines.length === 0, badLines[0]);
} catch (e) {
  console.error("TEST ERROR:", e.message ?? e);
  results.push({ name: "harness completed", ok: false });
} finally {
  p.kill();
}

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
process.exit(failed.length ? 1 : 0);
