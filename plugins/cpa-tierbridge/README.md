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

An object with a `candidates` array (a bare array works too), best first: the array order is the rank.
See `testdata/router.sample.json` and the package comment of `internal/routerfile`.

```json
{"version": 1, "candidates": [
  {"provider": "openai", "model": "gpt-4.1", "contextWindow": 1047576,
   "supportsTools": true, "input": ["text", "image"], "rpm": 500, "rpd": null}
]}
```

A candidate fits when the estimated prompt (text bytes / 4) is within `contextWindow` (null = unknown =
fits), it supports tools if the request declares any, it takes images if the request has any, its
provider is in the host's `AvailableProviders` (when that list is non-empty), and its `rpm`/`rpd` quota and
the daily budget allow one more request. `Reason` reads `rank N of M`, plus the skipped candidates.

## Status of the slice

Increment 1 (this): skeleton, ModelRouter, in-memory `rpm`/`rpd`/budget limiter behind the `Limiter`
interface. The `usage_plugin` and `management_api` capabilities are declared and answered with
no-ops/empty registrations; usage accounting, persistence and the management status route come next.
