// Persistent Devin ACP client. One `devin acp` child serves many prompts —
// no process spawn per turn, and streamed session/update events mean the
// caller sees tokens live instead of polling logs or the sessions DB.
//
//   node acp-bridge.mjs --cwd <dir> --prompt "..."            one turn, exit
//   node acp-bridge.mjs --cwd <dir> --session <id> --prompt … resume a session
//   node acp-bridge.mjs --cwd <dir> --model opus --prompt …
//
// Streamed prose/thought go to stdout live (orchestrator logs them the same
// as before); the result line `ACPRESULT {...json}` is written last, for
// programmatic callers to parse.
//
// The server asks the client to do the real work — files, terminals and
// permission prompts — so this implements that surface (fs read/write,
// terminal create/output/wait/kill/release, and auto-approve permissions).
// Permissions are approved because orchestrator agents run unattended —
// the same trust level `devin -p` already had.
import { spawn, execFileSync } from "node:child_process";
import { createInterface } from "node:readline";
import fs from "node:fs";
import path from "node:path";

// Lazy: importing this file as a library must not depend on `where` existing.
function resolveDevin() {
  if (process.env.DEVIN_BIN) return process.env.DEVIN_BIN;
  try {
    const found = process.platform === "win32"
      ? execFileSync("where", ["devin.exe", "devin.com"], { encoding: "utf8", windowsHide: true })
          .split(/\r?\n/).map((s) => s.trim()).find((s) => s.endsWith("devin.exe"))
      : execFileSync("which", ["devin"], { encoding: "utf8", windowsHide: true }).split("\n")[0];
    if (found) return found;
  } catch {}
  return "devin";
}

// Same credential handling as the orchestrator: ACP vars mean "forward this
// session's auth"; stripped, the child uses stored CLI credentials.
function cleanEnv() {
  const env = { ...process.env };
  for (const k of Object.keys(env)) if (/^(ACP_|CURSOR_|WINDSURF_)/.test(k)) delete env[k];
  return env;
}

// ---- terminals the agent asks us to run ----
const terminals = new Map();
function termCreate(id, params) {
  const cwd = typeof params.cwd === "string" && fs.existsSync(params.cwd) ? params.cwd : process.cwd();
  const shell = process.env.ComSpec || "cmd.exe";
  // ACP sends {command, args[], cwd, env[]}: join argv back into one command
  // line. Passed unquoted to /c — wrapping the whole line in quotes makes
  // cmd treat `&&`-joined commands as a single literal filename.
  const parts = [params.command, ...(Array.isArray(params.args) ? params.args : [])].filter(Boolean);
  const line = parts.map((a) => String(a)).join(" ").trim();
  const env = cleanEnv();
  for (const e of params.env ?? [])
    if (e?.name != null) env[String(e.name)] = String(e.value ?? "");
  const p = spawn(shell, ["/d", "/s", "/c", line], {
    cwd, env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
  });
  const t = { p, out: "", code: null, exited: false, id: `t${Math.random().toString(36).slice(2)}` };
  terminals.set(t.id, t);
  const cap = () => { if (t.out.length > 1 << 22) t.out = t.out.slice(-(1 << 22)); };
  p.stdout.on("data", (d) => { t.out += d; cap(); });
  p.stderr.on("data", (d) => { t.out += d; cap(); });
  // Signal kills give (null, "SIGKILL") — record exited separately or
  // wait_for_exit can never fire and the agent's turn deadlocks.
  p.on("exit", (c) => { t.code = c ?? -1; t.exited = true; });
  p.on("error", () => { t.code = -1; t.exited = true; });
  // Unref'd: a still-running command must not pin the bridge process open —
  // an exit-time execFile timeout would then report a good send as failure.
  p.unref(); p.stdout.unref(); p.stderr.unref();
  return { terminalId: t.id };
}
const termOut = (t, params) => {
  let out = t.out;
  let truncated = false;
  const limit = Number(params?.outputByteLimit) || 0;
  if (limit > 0 && out.length > limit) { out = out.slice(-limit); truncated = true; }
  const r = { output: out, truncated };
  if (t.exited) r.exitStatus = { exitCode: t.code };
  return r;
};

// ---- fs the agent asks us to do ----
const MAX_FILE = 4 << 20; // refuse to materialize bigger files in one reply
function fsRead(params) {
  const size = fs.statSync(params.path).size;
  const fd = fs.openSync(params.path, "r");
  try {
    const buf = Buffer.alloc(Math.min(size, MAX_FILE));
    fs.readSync(fd, buf, 0, buf.length, 0);
    const data = buf.toString("utf8");
    const lines = data.split("\n");
    const line = Math.max(1, Number(params.line) || 1);
    const limit = Number(params.limit) || 0;
    let content = limit > 0 ? lines.slice(line - 1, line - 1 + limit).join("\n") : data;
    if (size > MAX_FILE) content += `\n… [truncated: file is ${size} bytes, showing first ${MAX_FILE}]`;
    return { content };
  } finally { fs.closeSync(fd); }
}

export class AcpBridge {
  #p; #pending = new Map(); #seq = 0; #onUpdate;
  #ready;

  constructor({ onUpdate } = {}) {
    this.#onUpdate = onUpdate || (() => {});
    this.#p = spawn(resolveDevin(), ["acp"], { stdio: ["pipe", "pipe", "inherit"], env: cleanEnv(), windowsHide: true });
    const dead = () => { for (const { rej } of this.#pending.values()) rej(new Error("devin acp exited")); this.#pending.clear(); };
    this.#p.on("exit", dead);
    this.#p.on("error", dead);
    this.#p.stdin.on("error", () => {}); // EPIPE on a dead child must not crash us
    const rl = createInterface({ input: this.#p.stdout });
    rl.on("line", (l) => this.#line(l));
    this.#ready = this.#call("initialize", {
      protocolVersion: 1,
      clientCapabilities: { fs: { readTextFile: true, writeTextFile: true }, terminal: true },
      clientInfo: { name: "devin-acp-bridge", version: "0.1.0" },
    });
  }

  #write(m) { try { this.#p.stdin.write(JSON.stringify(m) + "\n"); } catch {} }
  get alive() { return this.#p.exitCode == null && !this.#p.killed; }
  #call(method, params) {
    // A dead child's stdin swallows writes silently — reject instead of pending forever.
    if (!this.alive) return Promise.reject(new Error("devin acp not running"));
    const id = ++this.#seq;
    return new Promise((res, rej) => { this.#pending.set(id, { res, rej }); this.#write({ jsonrpc: "2.0", id, method, params }); });
  }
  cancel(sessionId) { this.#write({ jsonrpc: "2.0", method: "session/cancel", params: { sessionId } }); }
  #reply(id, result) { this.#write({ jsonrpc: "2.0", id, result }); }
  #fail(id, code, message) { this.#write({ jsonrpc: "2.0", id, error: { code, message } }); }

  // The agent can only read/write under the session cwd or the repo —
  // its prompt is untrusted enough to cap where fs calls may land.
  #allowPaths = [];
  // Normalize with path.resolve so `allowed\..\secrets` can't escape.
  setAllowedDirs(dirs) { this.#allowPaths = dirs.map((d) => path.resolve(d).toLowerCase()); }
  #pathAllowed(p) {
    const norm = path.resolve(p).toLowerCase();
    return this.#allowPaths.length === 0
      || this.#allowPaths.some((d) => norm === d || norm.startsWith(d + path.sep));
  }

  async #request(m) {
    const { id, method, params } = m;
    try {
      if (method === "fs/read_text_file")
        return this.#reply(id, this.#pathAllowed(params.path) ? fsRead(params) : (() => { throw new Error("path outside allowed dirs"); })());
      if (method === "fs/write_text_file") {
        if (!this.#pathAllowed(params.path)) throw new Error("path outside allowed dirs");
        const dir = path.dirname(params.path);
        if (dir && dir !== ".") fs.mkdirSync(dir, { recursive: true });
        fs.writeFileSync(params.path, params.content ?? "");
        return this.#reply(id, {});
      }
      if (method === "terminal/create") return this.#reply(id, termCreate(id, params));
      if (method === "terminal/output") return this.#reply(id, termOut(terminals.get(params.terminalId) ?? { out: "", code: null }, params));
      if (method === "terminal/wait_for_exit") {
        const t = terminals.get(params.terminalId);
        if (!t) throw new Error("unknown terminal");
        if (!t.exited) await new Promise((r) => t.p.once("exit", r));
        return this.#reply(id, { exitCode: t.code });
      }
      // release frees OUR resources too — release-and-leave-running orphans the command
      if (method === "terminal/release") {
        const t = terminals.get(params.terminalId);
        terminals.delete(params.terminalId);
        if (t && !t.exited) try { t.p.kill("SIGKILL"); } catch {}
        return this.#reply(id, {});
      }
      if (method === "terminal/kill") { const t = terminals.get(params.terminalId); if (t && !t.exited) try { t.p.kill("SIGKILL"); } catch {} return this.#reply(id, {}); }
      if (method === "session/request_permission") {
        // Unattended: pick the least-destructive allow option, else the first.
        const opts = params?.options ?? [];
        const pick = opts.find((o) => /allow.*(once|always)|proceed/i.test(o.kind ?? o.optionId ?? ""))
          ?? opts.find((o) => !/reject|deny|cancel/i.test(o.kind ?? o.optionId ?? "")) ?? opts[0];
        return this.#reply(id, { outcome: { outcome: "selected", optionId: pick?.optionId ?? "allow_once" } });
      }
      // Unknown client-bound request: empty success is safer than dropping it
      // (a request left hanging stalls the turn); log-only methods fit _cognition.
      this.#reply(id, {});
    } catch (e) {
      this.#fail(id, -32603, String(e.message ?? e));
    }
  }

  #line(l) {
    let m; try { m = JSON.parse(l); } catch { return; }
    if (m.id != null && m.method != null) return void this.#request(m);        // server → client request
    if (m.id != null) {                                                       // response to our call
      const h = this.#pending.get(m.id); this.#pending.delete(m.id);
      if (!h) return;
      m.error ? h.rej(new Error(`${m.error.code}: ${m.error.message}`)) : h.res(m.result);
      return;
    }
    if (m.method) this.#onUpdate(m);                                          // notification
  }

  // Modes are per-session — key them by id so --parallel siblings can't
  // overwrite each other's advertised list between await hops.
  #modesById = new Map();
  modesFor(sessionId) { return this.#modesById.get(sessionId) ?? null; }

  async session(cwd, { sessionId } = {}) {
    await this.#ready;
    if (sessionId) {
      // session/load replies with modes/config only — the id is the one we sent.
      const r = await this.#call("session/load", { sessionId, cwd, mcpServers: [] });
      this.#modesById.set(sessionId, r?.modes ?? null);
      return sessionId;
    }
    const r = await this.#call("session/new", { cwd, mcpServers: [] });
    this.#modesById.set(r.sessionId, r?.modes ?? null);
    return r.sessionId;
  }

  // session/set_mode isn't universal — callers must treat rejection as
  // "mode unsupported" and continue, never fail the turn over it.
  async setMode(sessionId, modeId) {
    await this.#ready;
    return this.#call("session/set_mode", { sessionId, modeId });
  }

  // No rename/set-title method exists on this server build (verified:
  // session/rename, set_title, update_title, rename_session, setName all
  // -32601). Session titles come from the CLI's own prompt-based titling.

  async list(cwd) {
    await this.#ready;
    return this.#call("session/list", cwd ? { cwd } : {});
  }

  async prompt(sessionId, text) {
    await this.#ready;
    return this.#call("session/prompt", { sessionId, prompt: [{ type: "text", text }] });
  }

  async close() {
    try { this.#p.kill(); } catch {}
    // Terminals outlive the ACP child otherwise — they'd pin the event loop
    // (unref'd pipes prevent that) AND leak as orphans.
    for (const t of terminals.values()) { if (!t.exited) try { t.p.kill("SIGKILL"); } catch {} }
    terminals.clear();
  }
}

if (process.argv[1]?.endsWith("acp-bridge.mjs")) {
  const argv = process.argv.slice(2);
  const flag = (n) => {
    const i = argv.indexOf("--" + n);
    return i >= 0 && i + 1 < argv.length && !argv[i + 1].startsWith("--") ? argv[i + 1] : null;
  };
  const cwd = flag("cwd") || process.cwd();
  const text = flag("prompt") ?? (flag("prompt-file") ? fs.readFileSync(flag("prompt-file"), "utf8") : null);
  if (!text) { console.error("usage: acp-bridge.mjs --cwd dir --prompt text | --prompt-file f [--session id]"); process.exit(1); }
  const bridge = new AcpBridge({ onUpdate: (m) => {
    if (m.method !== "session/update") return;
    const u = m.params?.update ?? {};
    const c = u.content;
    if ((u.sessionUpdate === "agent_message_chunk" || u.sessionUpdate === "agent_thought_chunk") && c?.type === "text")
      process.stdout.write(c.text);
  }});
  try {
    bridge.setAllowedDirs([cwd]); // fs requests may not escape the session dir
    const sid = await bridge.session(cwd, { sessionId: flag("session") });
    const r = await bridge.prompt(sid, text);
    process.stdout.write(`\nACPRESULT ${JSON.stringify({ sessionId: sid, ...r })}\n`);
    process.exitCode = 0;
  } catch (e) {
    console.error("ACP failed:", e.message);
    process.exitCode = 1;
  } finally { bridge.close(); }
}
