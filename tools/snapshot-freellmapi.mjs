#!/usr/bin/env node
// Builds a `tierbridge-snapshot/1` envelope for the `freellmapi` source.
//
//   node --no-warnings tools/snapshot-freellmapi.mjs --repo <checkout> [--rev <rev>] --out <file>
//
// Why a script: freellmapi's open model registry is not a data file. It is the result of a chain of
// migrations (a seed list, then renames, UPDATEs and DELETEs over 25 data steps), so reading the
// literals out of the TypeScript would silently disagree with what the project actually serves.
// The script therefore REPLAYS the migrations into an empty in-memory SQLite database and dumps the
// resulting registry tables (models, embedding_models, quirks, quirk_targets).
//
// The 55 migrations registered in DEFAULT_MIGRATIONS (db/migrate/defaults.ts) are replayed in that
// order, as the project's own sync runner does. The directory holds a 56th file,
// 20261028_000001_custom_media_endpoint_identity.ts, that is not registered there; it concerns
// custom media endpoints only (media_models is empty in the seed).
// What runs, and what cannot happen:
//   * Sources come from `git show <sha>:<path>` at the pinned commit: only the migration files, the
//     migration list (`db/migrate/defaults.ts`) and `db/model-pricing.ts`. Nothing else is read.
//   * TypeScript is stripped with node:module `stripTypeScriptTypes` (no compiler, no network) and
//     evaluated in a `node:vm` context that exposes only the in-memory database and the stubs
//     (see STUBS). The context has no `require`, real `process`, `fs`, `fetch` or timers.
//   * The baseline's `initEncryptionKey` (which would write a key file) is a no-op; `crypto` is a
//     frozen subset of node:crypto's pure functions; `applyModelPricing` is the project's own.
//   * Run it under Node's permission model to enforce the above, e.g.
//       node --no-warnings --permission --allow-fs-read=<repo> --allow-fs-write=<out dir> \
//            --allow-child-process tools/snapshot-freellmapi.mjs ...
//     (child processes are needed only for `git show`).
//
// Needs Node >= 24 (node:sqlite, stripTypeScriptTypes).

import { execFileSync } from 'node:child_process';
import { Buffer } from 'node:buffer';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { DatabaseSync } from 'node:sqlite';
import vm from 'node:vm';

const SOURCE_ID = 'freellmapi';
const REPOSITORY = 'https://github.com/tashfeenahmed/freellmapi';
const MIGRATIONS_DIR = 'server/src/db/migrations/';
const DEFAULTS_FILE = 'server/src/db/migrate/defaults.ts';
const PRICING_FILE = 'server/src/db/model-pricing.ts';

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i === -1 ? undefined : process.argv[i + 1];
}

function git(repo, ...args) {
  return execFileSync('git', ['-C', repo, ...args], {
    encoding: 'utf8',
    maxBuffer: 1 << 28,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

const utc = (iso) => new Date(iso).toISOString().replace(/\.\d{3}Z$/, 'Z');

/** The 56 migration files in the order the project applies them (`DEFAULT_MIGRATIONS`). */
function migrationOrder(defaultsSource) {
  const names = new Map();
  for (const m of defaultsSource.matchAll(/export const (\w+_FILENAME)\s*=\s*'([^']+)'/g)) {
    names.set(m[1], m[2]);
  }
  const list = defaultsSource.slice(defaultsSource.indexOf('DEFAULT_MIGRATIONS'));
  const order = [...list.matchAll(/filename:\s*(\w+_FILENAME)/g)].map((m) => {
    if (!names.has(m[1])) throw new Error(`unknown migration constant ${m[1]}`);
    return names.get(m[1]);
  });
  if (order.length === 0) throw new Error('no migrations found in defaults.ts');
  return order;
}

/** TypeScript -> a function body that evaluates to the module's exports. */
function compile(source, filename, exportNames) {
  let js;
  try {
    js = stripTypeScriptTypes(source, { mode: 'strip' });
  } catch {
    js = stripTypeScriptTypes(source, { mode: 'transform' });
  }
  // Imports are the only external references: type imports are gone after stripping, and the
  // value imports are provided as context globals (STUBS / pricing).
  js = js.replace(/^import\s[^;]*?from\s+['"][^'"]+['"];?[ \t]*$/gm, '');
  js = js.replace(/^export\s*\{[^}]*\};?[ \t]*$/gm, ''); // `export { a, b };` lists: the names stay local
  js = js.replace(/^export\s+(async\s+)?(function|const|let|class)\b/gm, '$1$2');
  if (/^\s*(import|export)\b/m.test(js)) throw new Error(`${filename}: unsupported module syntax left after stripping`);
  const exported = exportNames.map((n) => `${n}: typeof ${n} === 'undefined' ? undefined : ${n}`).join(', ');
  return new vm.Script(`(function () {\n${js}\n;return { ${exported} };\n})()`, { filename });
}

/** The project's `Db` interface (db/types.ts) over an in-memory node:sqlite database. */
function makeDb() {
  const sqlite = new DatabaseSync(':memory:');
  let depth = 0;
  const bind = (params) => params.map((p) => (typeof p === 'boolean' ? Number(p) : p));
  const statement = (sql) => {
    const stmt = sqlite.prepare(sql);
    return {
      get: (...p) => stmt.get(...bind(p)),
      all: (...p) => stmt.all(...bind(p)),
      run: (...p) => {
        const r = stmt.run(...bind(p));
        return { lastInsertRowid: r.lastInsertRowid, changes: Number(r.changes) };
      },
    };
  };
  return {
    sqlite,
    db: {
      prepare: statement,
      exec: (sql) => sqlite.exec(sql),
      transaction: (fn) => (...args) => {
        const point = `tb_sp_${depth++}`;
        sqlite.exec(`SAVEPOINT ${point}`);
        try {
          const result = fn(...args);
          sqlite.exec(`RELEASE ${point}`);
          return result;
        } catch (error) {
          sqlite.exec(`ROLLBACK TO ${point}`);
          sqlite.exec(`RELEASE ${point}`);
          throw error;
        } finally {
          depth--;
        }
      },
      pragma: (source) => (source.includes('=') ? sqlite.exec(`PRAGMA ${source}`) : sqlite.prepare(`PRAGMA ${source}`).all()),
      name: ':memory:',
      memory: true,
    },
  };
}

const repo = arg('repo');
const out = arg('out');
if (!repo || !out) {
  console.error('usage: snapshot-freellmapi.mjs --repo <checkout> [--rev <rev>] --out <file>');
  process.exit(2);
}
const sha = git(repo, 'rev-parse', '--verify', `${arg('rev') ?? 'HEAD'}^{commit}`).trim();
const show = (path) => git(repo, 'show', `${sha}:${path}`);

// STUBS: the only things a migration can reach besides the database.
const STUBS = Object.freeze({
  crypto: Object.freeze({ createHash, randomBytes, randomUUID }),
  Buffer, // pure data container, used to serialise rows a migration deletes
  initEncryptionKey: () => { }, // would create a key file; irrelevant to the registry
  console: Object.freeze({ log() { }, warn() { }, error() { }, info() { }, debug() { } }),
  // Read by the baseline's unified-key helper, which only chooses where to print a throwaway key.
  process: Object.freeze({
    env: Object.freeze({ NODE_ENV: 'production' }),
    stdout: Object.freeze({ isTTY: false, write() { } }),
  }),
});
const context = vm.createContext({ ...STUBS });

const pricing = compile(show(PRICING_FILE), PRICING_FILE, ['applyModelPricing', 'MODEL_PRICING']).runInContext(context);
context.applyModelPricing = pricing.applyModelPricing;

const order = migrationOrder(show(DEFAULTS_FILE));
const { sqlite, db } = makeDb();
for (const file of order) {
  const migration = compile(show(MIGRATIONS_DIR + file), file, ['up']).runInContext(context);
  if (typeof migration.up !== 'function') throw new Error(`${file}: no up()`);
  migration.up(db);
}

const tables = sqlite
  .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
  .all()
  .map((r) => r.name);
if (process.argv.includes('--list-tables')) {
  for (const t of tables) {
    const cols = sqlite.prepare(`PRAGMA table_info(${t})`).all().map((c) => c.name);
    const n = sqlite.prepare(`SELECT COUNT(*) AS n FROM ${t}`).get().n;
    console.error(`${t} (${n}): ${cols.join(', ')}`);
  }
  process.exit(0);
}

// Replay timestamps (`Date.now()` at run time) are not registry data; dropping them keeps the
// snapshot byte-for-byte reproducible for a given commit.
const VOLATILE = new Set(['created_at', 'created_at_ms', 'updated_at', 'updated_at_ms']);
const dump = {};
for (const table of (arg('tables') ?? 'models,embedding_models,quirks,quirk_targets').split(',')) {
  if (!tables.includes(table)) throw new Error(`replayed database has no table ${table}`);
  const columns = sqlite.prepare(`PRAGMA table_info(${table})`).all().map((c) => c.name).filter((c) => !VOLATILE.has(c));
  const rows = sqlite
    .prepare(`SELECT ${columns.join(', ')} FROM ${table} ORDER BY 1`)
    .all()
    .map((r) => ({ ...r }));
  dump[table] = { columns, rows };
}
const envelope = {
  format: 'tierbridge-snapshot/1',
  sourceId: SOURCE_ID,
  repository: REPOSITORY,
  commit: sha,
  commitDate: utc(git(repo, 'show', '-s', '--format=%cI', sha).trim()),
  replay: { migrations: order.length, pricing: 'paid_input_per_m/paid_output_per_m = db/model-pricing.ts MODEL_PRICING' },
  tables: dump,
};
writeFileSync(out, JSON.stringify(envelope));
console.error(`${SOURCE_ID}: ${sha} ${envelope.commitDate}, ${order.length} migrations replayed, ` +
  Object.entries(dump).map(([t, d]) => `${t}=${d.rows.length}`).join(' '));
