# Live deployment and rollback scripts (prepared, never run here)

`deploy-live.sh` and `rollback-live.sh` stage and undo the plugin in a CPA home. They were only ever
exercised against a fake home under `mktemp`; nobody has run them against the real instance.

- `deploy-live.sh --router-file PATH [--dylib PATH] [--cpa-home DIR] [--alias A] [--daily-budget-usd N]
  [--cooldown-seconds N] [--max-attempts N] [--deadline-seconds N] [--priority N] [--force] [--dry-run]`
  backs up `config.yaml` to `config.yaml.bak.<UTC ts>` (`cp -p`, never overwrites), copies the dylib to
  `<plugins.dir>/darwin/arm64/tierbridge-v<semver>.dylib` (a different existing file needs `--force`; the
  old one is kept in the state dir), writes a JSON-lines manifest (no secrets) to
  `${TB_DEPLOY_STATE:-$HOME/.local/state/llm-tierbridge}/deploy.<ts>.manifest`, and prints the YAML block for
  `plugins.configs`. `CPA_HOME` defaults to `$HOME/Library/Application Support/com.cpa.gui/cpa-core`.
  It reads only the structural keys of the `plugins:` block (`dir`, `enabled`, whether `configs.tierbridge`
  exists). It never edits `config.yaml` and never restarts or signals anything: the controller edits the
  config and (re)loads CPA. `--priority` defaults to a low `10`: the host orders its chain by priority
  descending, then id ascending, and our router only answers its alias.
- `rollback-live.sh [--manifest PATH] [--yes] [--dry-run]` (default: newest manifest) prints a count-only diff
  summary, and with `--yes` restores the backed-up `config.yaml` (`cp -p`) and removes only the dylib the manifest
  records, and only if its sha256 still matches (a file replaced with `--force` is put back). Restart/reload is
  the controller's job. Neither script touches the live config without that explicit step.
- `max-attempts` and `deadline-seconds` (flags `--max-attempts`, `--deadline-seconds`; defaults 3 and 120) are printed
  in the block; the plugin reads both. The router file must be `tierbridge-router/2` host mode, generated with
  `export cpa-router --model-prefix ''` (live ids).

## Self-test

```sh
plugins/cpa-tierbridge/scripts/test-deploy.sh --self-test
shellcheck -x plugins/cpa-tierbridge/scripts/{deploy-live,rollback-live,test-deploy}.sh
```

`--self-test` builds a fake `cpa-core` (path with a space, `config.yaml` with a `plugins:` block and a fake
secret, a dummy dylib) under `mktemp`, redirects `HOME` and `TB_DEPLOY_STATE` into it, and asserts: `--dry-run`
touches nothing; deploy backs up (mode kept), copies the dylib and writes the manifest while `config.yaml` stays
unchanged; an existing backup and a different dylib are refused (no side effects) until `--force`; rollback
without `--yes` or with `--dry-run` changes nothing; rollback restores the config and removes or restores only
the recorded dylib, leaves a changed one alone, and never removes one that deploy did not install; the secret
never reaches the output; invalid flags are rejected. `TB_DEPLOY_TS` (format `20260101T000000Z`) overrides the
timestamp and exists for the test.
