# cpa-tierbridge

CLIProxyAPI native plugin (c-shared library) that adds the `tierbridge/auto` model alias. It is a
`ModelRouter`: for a request to the alias it picks the first candidate of a ranking file that fits the
request and hands `{provider, model}` back to the host's built-in auth/executor path. It is also a
usage plugin (per-model counters, budget, cooldown) and serves read-only management routes. Every
other model, and every plugin-side failure, is answered `Handled:false`, so the host carries on as if
the plugin were not there; for the alias itself that means the host answers `400 model_not_found`.

There is no `QuotaProvider`: the host's quota API is per credential (`auth_index`), while the plugin's
limits are per model. They are exposed through the management routes instead.

**Host pin:** CLIProxyAPI `47f18e4d82bd72f7d2b5ecf5ede93e4837a4625e` (contained in v8.1.7), native
ABI version 1, RPC schema version 10 (`sdk/pluginabi/types.go`). The ABI changes often upstream:
rebuild and re-verify against every host version you deploy. The wire types are a hand-written mirror
(`internal/wire`, no CLIProxyAPI import, no `replace`); golden JSON tests pin their shapes.

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

## Configuration (`plugins.configs.tierbridge`)

| key | default | meaning |
| --- | --- | --- |
| `router-file` | empty | path of `tierbridge-router.json`; reloaded when its mtime or size changes; empty = alias never handled |
| `alias` | `tierbridge/auto` | model name the plugin answers to |
| `daily-budget-usd` | `0` | estimated spend allowed per UTC day; `0` = no budget |
| `cooldown-seconds` | `60` | how long a model is skipped after a failed usage record (429 or >= 500); a larger `Retry-After` wins |
| `state-path` | empty | file the counters, spend and cooldowns persist to; empty = no persistence |
| `decisions-keep` | `50` | size of the recent-decisions ring (0 to 10000) |
| `debug-force-error` | `false` | TEST SWITCH ONLY: routing panics internally, to prove the host falls through |

An invalid configuration or router file never breaks routing: the previous good version keeps serving
and the error is reported by `/tierbridge/status`.

## Router file (`tierbridge-router/1`)

Written by `tierbridge export cpa-router`:
`{schema, catalogSha256, quotaSourceSha256, gateway, aliases:[{alias, profile, skipped, candidates:[...]}]}`.
The profile is per alias and the provider per candidate; the candidates of an alias are best first (the
array order is the rank); an alias may have no candidates (valid: every route then falls through with a
reason). Unknown keys are ignored at every level. The plugin serves the alias entry whose name equals the
configured `alias`. See `testdata/router.sample.json` and the package comment of `internal/routerfile`
for every field.

A candidate routes to `Target = provider`, `TargetModel = model` (the name the provider knows, i.e. the
exported openai-compatibility alias; `id` and `key` are informational). Counters are keyed by `model`.

## Routing

A candidate is passed over, in this order, when: the estimated prompt (text bytes / 4) exceeds
`contextWindow` (`context`; null = unknown = fits); the request declares tools and `supportsTools` is not
true (`tools`); it carries an image and `input` lacks `image` (`image`); the provider is not in the
host's `AvailableProviders` when that list is non-empty (`provider`); the model is cooling down
(`cooldown`); the estimated prompt cost would take the day's spend over `daily-budget-usd` (`budget`;
free candidates are never skipped for budget, an unknown price counts 0); `quota.rpm`/`quota.rpd` are
used up (`rpm`, `rpd`; reserved at route time); or the estimated prompt tokens do not fit the remaining
`quota.tpm`/`quota.tpd` (`tpm`, `tpd`). A null or 0 limit is unlimited. `Reason` reads `rank N of M` plus
the skipped candidates.

## Usage accounting

`usage.handle` always answers success, whatever the record holds. A record is matched to a candidate by
`Model` == candidate `model`, then `Alias` == candidate `model`, then `Model` == candidate `id` (the host
reports the upstream id without the exported prefix) when the record's provider is empty or agrees with
the candidate's; ambiguous or unknown models are ignored. A match charges tokens (`TotalTokens`, else
input + output) to tpm/tpd, spend `(InputTokens*cost.input + OutputTokens*cost.output)/1e6` to the day,
and, for `Failed` with status 429 or >= 500, starts a cooldown. Candidates with unknown price are
`unpriced`. The latest 20 records seen (matched or not) are kept as `lastUsageModels`.

`state-path`: an atomic JSON write (temporary file + rename, mode 0600) at most every 5 s after a change
and on `plugin.shutdown`; loaded on start and reconfigure with expired windows dropped. A corrupted file
starts empty and is reported in the status. Decisions are not persisted.

## Management API

Read-only `GET` routes, behind the host's management authentication, no menu entries. Bodies are JSON;
an unknown path answers 404 `{"error":"not found"}`, a non-GET method 405, a panic 500.

| path | content |
| --- | --- |
| `/tierbridge/status` | `plugin{name,version,abiVersion,schemaVersion,hostCommit}`, `router{path,revision{catalogSha256,quotaSourceSha256,fileSha256,modTime,profile,loadedAt},aliases,lastLoadError}`, `config`, `configError`, `lastError`, `lastErrorAt`, `lastUsageModels[{time,model,alias,provider,failed,status,tokens,matched}]`, `persistence{enabled,path,loaded,loadedAt,loadError,dirty,saves,lastSaveAt,saveError}` |
| `/tierbridge/candidates` | `alias`, `profile`, ordered `candidates[{rank,provider,model,id,contextWindow,maxTokens,supportsTools,input,cost,unpriced,limits{rpm,rpd,tpm,tpd}}]` (`null` limit = unlimited) |
| `/tierbridge/counters` | `day`, `now`, `models[{provider,model,rpm,rpd,tpm,tpd,cooldownRemainingSeconds}]`, each counter `{used,limit}` |
| `/tierbridge/budget` | `day`, `dailyBudgetUsd`, `spentUsd`, `remainingUsd` (`null` without budget), `unpriced[]` |
| `/tierbridge/decisions?limit=N` | `limit`, newest first `decisions[{time,requestedModel,chosen{provider,model,rank}\|null,reason,skipped[{rank,provider,model,reason}],estimatedTokens}]` (default 20) |

Request bodies and headers are never echoed. The host decides the full URL; the plugin matches the
request path by suffix.

## Scratch acceptance

`scripts/scratch-acceptance.sh` starts a throwaway CLIProxyAPI on `127.0.0.1:18417` (temporary directory
for cwd, config, plugins dir, router file and state file), loads only this plugin, sends at most 10 tiny
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
