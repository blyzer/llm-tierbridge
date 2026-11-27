# cpa-tierbridge

CLIProxyAPI native plugin (c-shared library) that adds the `tierbridge/auto` model alias. It is a
`ModelRouter`: for requests to the alias it picks the first candidate of a ranking file that fits the
request and hands `{provider, model}` back to the host's built-in auth/executor path. Every other model,
and every plugin-side failure, is answered `Handled:false` so the host carries on as if the plugin were
not there.

**Host pin:** CLIProxyAPI `47f18e4d82bd72f7d2b5ecf5ede93e4837a4625e` (contained in v8.1.7), native
ABI version 1, RPC schema version 10 (`sdk/pluginabi/types.go`). The ABI changes often upstream:
rebuild and re-verify against every host version you deploy. The wire types are a hand-written mirror
(`internal/wire`, no CLIProxyAPI import, no `replace`); the golden JSON tests pin their shapes.

## Build

```sh
make check   # go vet, gofmt, go test
make dylib   # dist/tierbridge-v0.1.0.dylib (darwin/arm64, the exact CLIProxyAPI examples flags)
```

`dist/` is git-ignored: a built library is never committed.

## Configuration (`plugins.configs.tierbridge`)

| key | default | meaning |
| --- | --- | --- |
| `router-file` | empty | path of `tierbridge-router.json`; reloaded when its mtime or size changes; empty = alias never handled |
| `alias` | `tierbridge/auto` | model name the plugin answers to |
| `daily-budget-usd` | `0` | spend allowed per UTC day; `0` = no budget |
| `cooldown-seconds` | `60` | skip time after an upstream failure (usage plugin) |
| `state-path` | empty | persistence file for counters and decisions; empty = none |
| `decisions-keep` | `50` | size of the recent-decisions ring |
| `debug-force-error` | `false` | TEST SWITCH ONLY: routing panics internally, to prove the host falls through |

An invalid configuration or router file never breaks routing: the previous good version keeps serving
and the error is kept for the management status (`Handler.Status()`).

## Router file

`tierbridge-router.json`, schema `tierbridge-router/1`, written by `tierbridge export cpa-router`:
`{schema, catalogSha256, quotaSourceSha256, gateway, aliases:[{alias, profile, skipped, candidates:[...]}]}`.
The profile is per alias and the provider per candidate; the candidates of an alias are best first (the
array order is the rank); an alias may have no candidates (valid: every route then falls through with a
Reason). Unknown keys are ignored at every level. The plugin serves the alias entry whose name equals
the configured `alias`; a file without it falls through with `router file has no alias "<alias>"`.
See `testdata/router.sample.json` and the package comment of `internal/routerfile` for every field.

Per candidate the plugin routes to `Target = provider`, `TargetModel = model` (the name the provider
knows, i.e. the exported openai-compatibility alias; `id`/`key` are informational). A candidate fits when
the estimated prompt (text bytes / 4) is within `contextWindow` (null = unknown = fits), it supports tools
if the request declares any (`supportsTools` null = no), it takes images if the request has any
(`input` null = text only), its provider is in the host's `AvailableProviders` (when that list is
non-empty), and its `quota.rpm`/`quota.rpd` and the daily budget allow one more request. `Reason` reads
`rank N of M`, plus the skipped candidates. Candidates with `cost.status` unknown (or null prices) are
unpriced: budget math counts them as 0 and the status flags them (`aliases[].unpriced`,
`decisions[].unpriced`). The status revision is `catalogSha256` plus the file's sha256 and mtime.

## Status of the slice

Increment 1 (this): skeleton, ModelRouter, in-memory `rpm`/`rpd`/budget limiter behind the `Limiter`
interface. The `usage_plugin` and `management_api` capabilities are declared and answered with
no-ops/empty registrations; usage accounting, persistence and the management status route come next.
