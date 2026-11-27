#!/usr/bin/env node
// Builds a `tierbridge-snapshot/1` envelope from a git checkout, for the sources whose catalog is
// a few JSON files in a repository: router-for-me.models and dsh.
//
//   node tools/snapshot-git.mjs <sourceId> --repo <checkout> [--rev <rev>] --out <file>
//
// Reads blobs with `git show <sha>:<path>` at the pinned commit; it never touches the working tree,
// the network or any other file of the project. Only the catalog files listed in SOURCES are read.

import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';

const SOURCES = {
  // What CLIProxyAPI serves. No LICENSE upstream: facts only (the Codex template is projected down
  // to its facts; its prompt text is not copied).
  'router-for-me.models': {
    repository: 'https://github.com/router-for-me/models',
    rev: 'HEAD',
    files: [
      'models.json',
      'devin_models.json',
      'codex_client_models.json',
      'native-capabilities-evidence.json',
    ],
    project: { 'codex_client_models.json': projectCodexTemplate },
    // No file marks a model as retired; removal from these files is the only signal.
    retirementsFrom: ['models.json', 'devin_models.json'],
  },
  // Only the aggregate (bring-your-own-key) provider registry. Not vendor/channel-pack, not any
  // account or pool code; `catalog/` and `feed/` hold release integrity data and announcements.
  dsh: {
    repository: 'https://github.com/Ebony-Vinyl/dsh-our-free-model',
    rev: 'origin/main',
    files: ['src/providers/registry.json'],
  },
};

const CODEX_KEEP = [
  'slug', 'display_name', 'context_window', 'max_context_window', 'default_reasoning_level',
  'input_modalities', 'available_in_plans', 'visibility', 'supported_in_api',
  'supports_search_tool', 'supports_parallel_tool_calls', 'upgrade',
];

function projectCodexTemplate(json) {
  return {
    models: json.models.map((m) => {
      const out = {};
      for (const key of CODEX_KEEP) if (key in m) out[key] = m[key];
      out.supported_reasoning_levels = (m.supported_reasoning_levels ?? []).map((l) =>
        typeof l === 'string' ? l : l.effort,
      );
      return out;
    }),
  };
}

function git(repo, ...args) {
  return execFileSync('git', ['-C', repo, ...args], {
    encoding: 'utf8',
    maxBuffer: 1 << 30,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

const utc = (iso) => new Date(iso).toISOString().replace(/\.\d{3}Z$/, 'Z');

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i === -1 ? undefined : process.argv[i + 1];
}

function blob(repo, sha, path) {
  return JSON.parse(git(repo, 'show', `${sha}:${path}`));
}

/** (provider, id) -> model object for every channel list in the given catalog files at `sha`. */
function catalogState(repo, sha, files) {
  const state = new Map();
  for (const file of files) {
    let json;
    try {
      json = blob(repo, sha, file);
    } catch {
      continue; // the file did not exist (or was not valid JSON) at that commit
    }
    for (const [provider, list] of Object.entries(json)) {
      if (!Array.isArray(list)) continue;
      for (const model of list) {
        if (model && typeof model.id === 'string') state.set(`${provider}\u0000${model.id}`, { provider, id: model.id, model });
      }
    }
  }
  return state;
}

/** Models served at some earlier commit and absent at `sha`, with where they were last served/removed. */
function retirements(repo, sha, files) {
  const commits = git(repo, 'log', '--reverse', '--format=%H%x09%cI', sha, '--', ...files)
    .split('\n')
    .filter(Boolean)
    .map((l) => l.split('\t'));
  const last = new Map();
  const removed = new Map();
  let previous = new Set();
  for (const [commit, date] of commits) {
    const state = catalogState(repo, commit, files);
    for (const key of previous) {
      if (!state.has(key)) removed.set(key, { removedInCommit: commit, removedAt: utc(date) });
    }
    for (const [key, entry] of state) {
      last.set(key, { ...entry, lastServedCommit: commit });
      removed.delete(key);
    }
    previous = new Set(state.keys());
  }
  return [...removed.entries()]
    .map(([key, gone]) => {
      const { provider, id, model, lastServedCommit } = last.get(key);
      return { provider, id, model, lastServedCommit, ...gone };
    })
    .sort((a, b) => (a.provider + '\u0000' + a.id).localeCompare(b.provider + '\u0000' + b.id));
}

const sourceId = process.argv[2];
const spec = SOURCES[sourceId];
const repo = arg('repo');
const out = arg('out');
if (!spec || !repo || !out) {
  console.error(`usage: snapshot-git.mjs <${Object.keys(SOURCES).join('|')}> --repo <checkout> [--rev <rev>] --out <file>`);
  process.exit(2);
}
const sha = git(repo, 'rev-parse', '--verify', `${arg('rev') ?? spec.rev}^{commit}`).trim();
const envelope = {
  format: 'tierbridge-snapshot/1',
  sourceId,
  repository: spec.repository,
  commit: sha,
  commitDate: utc(git(repo, 'show', '-s', '--format=%cI', sha).trim()),
  files: {},
};
for (const file of spec.files) {
  const json = blob(repo, sha, file);
  envelope.files[file] = spec.project?.[file] ? spec.project[file](json) : json;
}
if (spec.retirementsFrom) envelope.retired = retirements(repo, sha, spec.retirementsFrom);
writeFileSync(out, JSON.stringify(envelope));
console.error(`${sourceId}: ${sha} ${envelope.commitDate}, ${spec.files.length} files` +
  (envelope.retired ? `, ${envelope.retired.length} retired` : ''));
