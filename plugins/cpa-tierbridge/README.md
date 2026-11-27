# cpa-tierbridge

CLIProxyAPI native plugin (c-shared library) that adds the `tierbridge/auto` model alias: one name that
runs a ranked list of models, free first, and falls back down the list inside the request.

## Why the plugin is its own executor

The live CLIProxyAPI has no `openai-compatibility` provider, and most of its models are served by other
plugins. A plain `ModelRouter` can only name a provider and a model, so it cannot reach them. The plugin
therefore has three roles:

- **ModelRouter**, for the alias only: it answers `TargetKind=self`; every other model gets
  `Handled:false` and the host carries on as if the plugin were not there. If no candidate fits, or
  anything fails inside the plugin (a forced error or a recovered panic), the router answers
  `Handled:false` too, and the host gives its normal reply for an unrouted alias: `400 model_not_found`.
- **Executor** (`executor.identifier` = `tierbridge`): the host calls it for the alias. It runs the ranked
  candidates through the host **by model name** (`host.model.execute`, and `host.model.execute_stream` for
  streams). The alias itself is never executed, so there is no recursion.
- **Usage plugin** and **management API** for per-model counters, budget and cooldown.

The host asks the router once per request and does not re-route when the chosen model fails, so the
fallback happens inside the executor. There is no `QuotaProvider`: the host's quota API is per credential
(`auth_index`), while the plugin's limits are per model; they are exposed through the management JSON.

`count_tokens` and `http_request` answer `not_implemented` (HTTP 501) with a message. They are not a
failure to cool, so the host keeps the model available.

**Host pin:** CLIProxyAPI `47f18e4d82bd72f7d2b5ecf5ede93e4837a4625e` (contained in v8.1.7), native
ABI version 1, RPC schema version 10. The ABI changes often upstream: rebuild and re-verify against every
host version you deploy. The wire types are a hand-written mirror (`internal/wire`, no CLIProxyAPI import,
no `replace`); golden JSON tests pin their shapes.

## In-request fallback

For each candidate in rank order (skipping those that do not fit, see below) the executor makes one
attempt, up to `max-attempts` attempts per request.

Non-streaming. After an attempt that fails with any of these, the candidate goes on cooldown and the next
one is tried:

- HTTP 429 or any 5xx;
- a transport failure or timeout;
- 401, 402, 403, 404, 408 or 425;
- `model_not_found` (HTTP 400 or 404 whose error code, body or message says so: the host does not serve that
  model).

Any other 4xx (400, 413, 422, ...) blames the request, not the candidate: it is returned to the client
unchanged. When every attempt failed (or `max-attempts` is reached), the last upstream error is returned
(502 for a transport failure; 503 when the last failure was `model_not_found`); a deadline gives 504.

Streaming. An attempt is settled only when the inner stream delivers its first output chunk. A start
status >= 400, or an error before the first chunk, moves to the next candidate. After the first chunk there
is **no fallback**: an error closes the stream with the error detail.

`deadline-seconds` bounds a non-streaming request, all attempts included, and for streams only the wait for
the first chunk.

A candidate fits when: the estimated prompt (text bytes / 4) is within `contextWindow` (null = unknown =
fits); it supports tools if the request declares any (`supportsTools` null = no); it takes images if the
request has any (`input` null = text only); it is not itself an alias name; it is not cooling down
(`cooldown`); the estimated prompt cost keeps the day's spend within `daily-budget-usd` (`budget`; free
candidates are never skipped for budget, an unknown price counts 0); `quota.rpm`/`quota.rpd` have room
(`rpm`, `rpd`; reserved when the executor picks the candidate); and the estimated prompt tokens fit the
remaining `quota.tpm`/`quota.tpd` (`tpm`, `tpd`). A null or 0 limit is unlimited. The router's `model.route`
only previews this and consumes nothing.

## Build and install

```sh
make check   # go vet, gofmt, go test
make dylib   # dist/tierbridge-v0.1.0.dylib (darwin/arm64, the exact CLIProxyAPI examples flags)
```

`dist/` is git-ignored: a built library is never committed. Install the library as

```
<plugins.dir>/darwin/arm64/tierbridge-v0.1.0.dylib
```

and enable it under `plugins.configs.tierbridge` in the host's `config.yaml`. The host adds its own
`enabled` and `priority` keys there; the plugin ignores them. Only the macOS/arm64 build is exercised.

`scripts/deploy-live.sh` and `scripts/rollback-live.sh` stage and undo a deployment in a CPA home. They are
**prepared and have never been run against the live CPA** (only against a fake home, `scripts/test-deploy.sh
--self-test`). They back up `config.yaml`, copy the dylib, print the YAML block for `plugins.configs` and do
not edit the config or restart anything. See `scripts/README-deploy.md`.

## Configuration (`plugins.configs.tierbridge`)

| key | default | meaning |
| --- | --- | --- |
| `router-file` | empty | path of `tierbridge-router.json`; reloaded when its mtime or size changes; empty = alias never handled |
| `alias` | `tierbridge/auto` | model name the plugin answers to |
| `daily-budget-usd` | `0` | estimated spend allowed per UTC day; `0` = no budget |
| `cooldown-seconds` | `60` | how long a model is skipped after a failed attempt or usage record (429, >= 500, model not found); a larger `Retry-After` wins |
| `max-attempts` | `3` | candidates one request may try (1 to 50) |
| `deadline-seconds` | `120` | see "In-request fallback"; `0` = no deadline |
| `state-path` | empty | file the counters, spend and cooldowns persist to; empty = no persistence |
| `decisions-keep` | `50` | size of the recent-decisions ring (0 to 10000) |
| `debug-force-error` | `false` | TEST SWITCH ONLY: the router fails internally, so the alias answers 400 `model_not_found` |

An invalid configuration or router file never breaks routing: the previous good version keeps serving
and the error is reported by `/tierbridge/status`.

## Router file (`tierbridge-router/2`)

Written by `tierbridge export cpa-router`
(`--catalog`, `--gateway`, `--model-prefix` [default empty], `--profile free-first`, `--alias`,
`--freellmapi-observations`, `--out`):
`{schema, target:"host", modelPrefix, catalogSha256, quotaSourceSha256, gateway, aliases:[{alias, profile, skipped, candidates:[...]}]}`.

`target: "host"` means the CPA that runs the plugin resolves every `candidate.model` itself; candidates carry
**no provider**. `model` is `modelPrefix` + the upstream `id`: the live CPA lists the plain ids (empty prefix);
a scratch CPA configured with `export cpa --alias-prefix tb-` needs `--model-prefix tb-`. The file is
generated locally and not committed. `tierbridge-router/1` (provider per candidate) is no longer
supported and is rejected with a clear error.

The candidates of an alias are best first (the array order is the rank); an alias may have no candidates
(valid: every route then falls through). Unknown keys are ignored. Quotas (`rpm`, `rpd`, `tpm`, `tpd`) come
from `--freellmapi-observations` (the merged catalog has no raw limits); without it they are null.
See `testdata/router.sample.json` and the package comment of `internal/routerfile` for every field.

## Usage accounting

`usage.handle` always answers success, whatever the record holds. Records of the executor's inner executions
carry `Model` = the upstream id and `Alias` = the candidate name the executor asked for. A record is matched
to a candidate by `Model` == candidate `model`, then `Alias` == candidate `model`, then `Model` == candidate
`id` (the host may report the upstream id without the exported prefix). Ambiguous or unknown models are
ignored. A match charges tokens (`TotalTokens`, else input + output) to tpm/tpd, spend
`(InputTokens*cost.input + OutputTokens*cost.output)/1e6` to the day, and, for `Failed` with status 429 or
>= 500, starts a cooldown. Candidates with unknown price are `unpriced`. The latest 20 records seen (matched or
not) are kept as `lastUsageModels`.

`state-path`: an atomic JSON write (temporary file + rename, mode 0600). A change is written at once when
the previous write is more than 1 s old, otherwise within 1 s of that write, so at most about one write per
second. Pending state is also flushed synchronously on `plugin.quiesce`, on `plugin.shutdown` and from the
C shutdown hook (`cliproxyPluginShutdown`); the host did not send `plugin.shutdown` when the process was
stopped. In the scratch acceptance a SIGTERM 0.2 s after a change still left the state file equal to the live counters,
so some shutdown path flushed; which one (the C hook or `plugin.quiesce`) is not visible because the plugin logs nothing
(without it, a stop can lose up to about 1 s of changes). Loaded on start and reconfigure with expired
windows dropped. A corrupted file starts empty and is reported in the status. Decisions are not persisted.

## Management API

Read-only `GET` routes, behind the host's management authentication, no menu entries. Bodies are JSON;
an unknown path answers 404 `{"error":"not found"}`, a non-GET method 405, a panic 500.

| path | content |
| --- | --- |
| `/tierbridge/status` | `plugin{name,version,abiVersion,schemaVersion,hostCommit}`, `router{path,revision{catalogSha256,quotaSourceSha256,fileSha256,modTime,profile,loadedAt},aliases,lastLoadError}`, `config`, `configError`, `lastError`, `lastErrorAt`, `lastUsageModels[{time,model,alias,provider,failed,status,tokens,matched}]`, `persistence{enabled,path,loaded,loadedAt,loadError,dirty,saves,lastSaveAt,saveError}` |
| `/tierbridge/candidates` | `alias`, `profile`, ordered `candidates[{rank,provider,model,id,contextWindow,maxTokens,supportsTools,input,cost,unpriced,limits{rpm,rpd,tpm,tpd}}]` (`null` limit = unlimited) |
| `/tierbridge/counters` | `day`, `now`, `models[{provider,model,rpm,rpd,tpm,tpd,cooldownRemainingSeconds}]`, each counter `{used,limit}` |
| `/tierbridge/budget` | `day`, `dailyBudgetUsd`, `spentUsd`, `remainingUsd` (`null` without budget), `unpriced[]` |
| `/tierbridge/decisions?limit=N` | `limit`, newest first `decisions[{time,requestedModel,chosen{model,rank,status}\|null,reason,skipped[{rank,model,reason}],estimatedTokens,attempts[{rank,model,status,ok,fallback,coolingDown,durationMs}],status}]` (default 20) |

Request bodies and headers are never echoed. The host decides the full URL; the plugin matches the
request path by suffix.

## Scratch acceptance

`scripts/scratch-acceptance.sh` starts a throwaway CLIProxyAPI on `127.0.0.1:18417` (temporary directory
for cwd, config, plugins dir, router file and state file), loads only this plugin, sends at most 12 tiny
chat requests, kills only the PID it started and removes the directory. It never touches a live
instance's config, plugins or process. Run it from the repository root:

```sh
REQUIRE_MGMT=1 FAIL_UPSTREAM_ID=<served id the upstream fails today> VERBOSE=1 \
  plugins/cpa-tierbridge/scripts/scratch-acceptance.sh
```

The upstream key comes from `EASYCLIPROXY_API_KEY` (or the macOS Keychain item
`security find-generic-password -s llm-tierbridge -a EASYCLIPROXY_API_KEY`). Without `PLUGIN_REF` it
builds the working tree; `PLUGIN_REF=HEAD` builds a git ref instead. The header of the script lists every
option (`CATALOG`, `GATEWAY`, `NAME`, `PORT`, `CPA_BIN`, `UPSTREAM`, `DYLIB`, `TB_BIN`, `ALIAS`,
`ALIAS_PREFIX`). It prints one PASS/FAIL/WARN/INFO line per check and exits 1 on any FAIL.

It checks, against a real host: the plugin loads and registers; the alias is served by the executor; an
rpm-1 candidate is skipped on the next request; in-request fallback with a nonexistent model first and with
an upstream 429 (one client call, answered by the next candidate); the cooldown on the next request; a
non-alias model is unaffected; a forced internal error gives 400 `model_not_found` with the host alive; the
usage records of inner executions; the five management routes (200 with the secret, 401 without);
persistence across a SIGTERM; and streaming, including fallback before the first chunk.

## Not verified

- Running on the live instance (nothing here was ever deployed to it).
- Interaction with the other live plugins' chains, i.e. which `priority` to choose (the host orders its
  chain by priority descending, then id ascending; the router only answers its own alias).
- A client disconnect in the middle of a stream while the inner read is blocked: it is noticed on the
  next chunk.
- The deadline covers only the wait for the first chunk of a stream.
- `count_tokens` (answers 501) and hot reload.
- Budget math with priced candidates and rpd/tpd limits in a real host (the acceptance candidates were free
  with an rpm limit only), and other platforms than macOS/arm64.
