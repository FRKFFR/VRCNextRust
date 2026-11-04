// Selection test for `orchestrate.mjs --prune-sessions`: builds a throwaway
// sessions.db (via DEVIN_SESSIONS_DB) and checks which rows a --dry-run
// targets. Dry-run only — never invokes `devin rm`.
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";

const here = import.meta.dirname;
const repoRoot = execFileSync("git", ["rev-parse", "--show-toplevel"], { cwd: here, encoding: "utf8" }).trim();
const tmp = mkdtempSync(join(tmpdir(), "prune-test-"));
const dbPath = join(tmp, "sessions.db");
const now = Date.now() / 1000;
const old = now - 40 * 86400;

const db = new DatabaseSync(dbPath);
db.exec("CREATE TABLE sessions (id TEXT, title TEXT, working_directory TEXT, last_activity_at REAL)");
const ins = db.prepare("INSERT INTO sessions VALUES (?, ?, ?, ?)");
ins.run("dead-dir", "t", join(tmp, "gone", "worktree"), now);        // ENOENT → target
ins.run("live-recent", "t", repoRoot, now);                          // keep
ins.run("repo-old", "t", repoRoot, old);                             // --days → target
ins.run("foreign-old", "t", tmp, old);                               // other project → keep
ins.run("-flagish", "t", join(tmp, "gone2"), now);                   // dead but invalid id → skipped
ins.run("no-dir", "t", null, now);                                   // keep
db.close();

const run = (...args) => spawnSync(process.execPath, [join(here, "orchestrate.mjs"), "--prune-sessions", "--dry-run", ...args],
  { cwd: repoRoot, encoding: "utf8", timeout: 60_000, env: { ...process.env, DEVIN_SESSIONS_DB: dbPath } });

const results = [];
const check = (name, cond, detail = "") => {
  results.push(!!cond);
  console.log(`${cond ? "PASS" : "FAIL"} ${name}${detail && !cond ? ` — ${detail}` : ""}`);
};
const targets = (out) => [...out.matchAll(/would rm (\S+)/g)].map((m) => m[1]).sort().join(",");

try {
  const a = run();
  check("dead-dir only without --days", targets(a.stdout) === "dead-dir", a.stdout + a.stderr);
  check("invalid id reported and skipped", /SKIP odd id "-flagish"/.test(a.stdout) && /1 invalid id/.test(a.stdout), a.stdout);
  check("dry run exits 0", a.status === 0, String(a.status));

  const b = run("--days", "30");
  check("--days adds old repo sessions, not foreign ones", targets(b.stdout) === "dead-dir,repo-old", b.stdout + b.stderr);

  const c = spawnSync(process.execPath, [join(here, "orchestrate.mjs"), "--prune-sessions", "--dry-run"],
    { cwd: repoRoot, encoding: "utf8", timeout: 60_000, env: { ...process.env, DEVIN_SESSIONS_DB: join(tmp, "missing.db") } });
  check("missing db → non-zero with clear message", c.status !== 0 && /not found/.test(c.stdout), c.stdout);
} finally {
  rmSync(tmp, { recursive: true, force: true });
}

const failed = results.filter((r) => !r).length;
console.log(`\n${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
