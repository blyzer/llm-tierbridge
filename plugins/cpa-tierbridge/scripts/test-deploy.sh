#!/usr/bin/env bash
# test-deploy.sh --self-test : exercises deploy-live.sh and rollback-live.sh against a FAKE CPA home
# created under mktemp. HOME and TB_DEPLOY_STATE are redirected into the temp dir, so the real
# CPA home and the real state dir cannot be reached. Nothing here starts or signals any process.
set -euo pipefail

if [[ ${1:-} != "--self-test" ]]; then
  echo "usage: test-deploy.sh --self-test" >&2
  exit 2
fi

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
deploy="$here/deploy-live.sh"
rollback="$here/rollback-live.sh"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/tb-deploy-test.XXXXXX")
tmp=$(cd -- "$tmp" && pwd -P)
trap 'rm -rf -- "$tmp"' EXIT

# redirect everything the scripts could default to
export HOME="$tmp/home"
export TB_DEPLOY_STATE="$tmp/state"
unset CPA_HOME
mkdir -p "$HOME"

home="$tmp/Application Support/cpa-core"   # a space, like the live path
mkdir -p "$home"
case "$home" in "$tmp"/*) ;; *) echo "FATAL: fake home escaped the temp dir" >&2; exit 3 ;; esac

SECRET="sk-SELFTEST-SECRET-0123456789"
cat >"$home/config.yaml" <<EOF
host: ""
port: 8317
api-keys:
  - "$SECRET"
plugins:
  enabled: true
  dir: "plugins"   # relative to cpa-core
  configs:
    other:
      enabled: true
EOF
chmod 600 "$home/config.yaml"
cp -p "$home/config.yaml" "$tmp/config.orig"

mkdir -p "$tmp/dist"
printf 'dummy dylib A\n' >"$tmp/dist/tierbridge-v0.1.0.dylib"
printf '{"schema":"tierbridge-router/2","aliases":[]}\n' >"$tmp/router.json"
target="$home/plugins/darwin/arm64/tierbridge-v0.1.0.dylib"

pass=0
fail() { echo "FAIL: $*" >&2; [[ -f ${out:-} ]] && sed -e 's/^/  | /' "$out" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "ok   $*"; }
mode_of() { stat -f %Lp "$1" 2>/dev/null || stat -c %a "$1"; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum -- "$1" | awk '{print $1}'; else shasum -a 256 -- "$1" | awk '{print $1}'; fi; }
same() { cmp -s -- "$1" "$2"; }
nobak() { local n; n=$(find "$home" -maxdepth 1 -name 'config.yaml.bak.*' | wc -l | tr -d ' '); [[ $n == "$1" ]] || fail "expected $1 backups, found $n"; }
out="$tmp/out.log"

run_deploy() { # ts args... ; stdout+stderr captured in $out
  local ts=$1; shift
  TB_DEPLOY_TS=$ts "$deploy" --cpa-home "$home" --dylib "$tmp/dist/tierbridge-v0.1.0.dylib" \
    --router-file "$tmp/router.json" "$@" >"$out" 2>&1
}
no_secret() { if grep -q -- "$SECRET" "$out"; then fail "secret leaked into output ($1)"; fi; }

# 1. dry-run touches nothing
run_deploy 20260101T000001Z --dry-run || fail "dry-run failed"
[[ ! -e $TB_DEPLOY_STATE ]] || fail "dry-run created the state dir"
[[ ! -e $home/plugins ]] || fail "dry-run created the plugins dir"
nobak 0
same "$home/config.yaml" "$tmp/config.orig" || fail "dry-run changed config.yaml"
grep -q 'tierbridge:' "$out" || fail "dry-run did not print the YAML block"
no_secret dry-run
ok "dry-run prints the block and touches nothing"

# 2. real deploy
run_deploy 20260101T000002Z --priority 7 --daily-budget-usd 2.5 --cooldown-seconds 45 --max-attempts 4 --deadline-seconds 90 --alias tb/x || fail "deploy failed"
no_secret deploy
same "$home/config.yaml" "$tmp/config.orig" || fail "deploy modified config.yaml"
b1="$home/config.yaml.bak.20260101T000002Z"
[[ -f $b1 ]] || fail "backup missing"
same "$b1" "$tmp/config.orig" || fail "backup differs from the original"
[[ $(mode_of "$b1") == "$(mode_of "$home/config.yaml")" ]] || fail "backup mode not preserved"
same "$target" "$tmp/dist/tierbridge-v0.1.0.dylib" || fail "dylib not installed"
m1="$TB_DEPLOY_STATE/deploy.20260101T000002Z.manifest"
[[ -f $m1 ]] || fail "manifest missing"
[[ $(jq -c . "$m1" | wc -l | tr -d ' ') == 3 ]] || fail "manifest should have 3 JSON lines"
[[ $(jq -r 'select(.kind=="dylib")|.sha256' "$m1") == "$(sha "$tmp/dist/tierbridge-v0.1.0.dylib")" ]] || fail "manifest dylib sha"
[[ $(jq -r 'select(.kind=="config")|.backupSha256' "$m1") == "$(sha "$tmp/config.orig")" ]] || fail "manifest backup sha"
for want in 'priority: 7' 'daily-budget-usd: 2.5' 'cooldown-seconds: 45' 'max-attempts: 4' 'deadline-seconds: 90' 'alias: "tb/x"' \
  "router-file: \"$tmp/router.json\"" "state-path: \"$TB_DEPLOY_STATE/tierbridge-state.json\""; do
  grep -qF -- "$want" "$out" || fail "YAML block lacks: $want"
done
ok "deploy: backup (mode kept), dylib copied, manifest written, config untouched"

# 3. same timestamp -> refuse to overwrite the backup
if run_deploy 20260101T000002Z; then fail "second deploy with the same ts should refuse"; fi
grep -q 'refusing to overwrite' "$out" || fail "no refuse message for existing backup"
ok "refuses to overwrite an existing backup"

# 4. different dylib already installed: refuse without --force, nothing created
printf 'dummy dylib B\n' >"$tmp/dist/tierbridge-v0.1.0.dylib"
if run_deploy 20260101T000003Z; then fail "overwrite without --force should fail"; fi
grep -q -- '--force' "$out" || fail "no --force hint"
[[ $(cat "$target") == "dummy dylib A" ]] || fail "refused deploy changed the dylib"
nobak 1
[[ ! -e $TB_DEPLOY_STATE/deploy.20260101T000003Z.manifest ]] || fail "refused deploy left a manifest"
ok "refuses to overwrite a different dylib without --force (no side effects)"

# 5. controller edits config, then --force deploy replaces and keeps the previous dylib
printf '    tierbridge:\n      enabled: true\n' >>"$home/config.yaml"
cp -p "$home/config.yaml" "$tmp/config.edited"
run_deploy 20260101T000004Z --force || fail "--force deploy failed"
[[ $(cat "$target") == "dummy dylib B" ]] || fail "--force did not replace the dylib"
[[ -f $TB_DEPLOY_STATE/deploy.20260101T000004Z.manifest ]] || fail "second manifest missing"
[[ $(cat "$TB_DEPLOY_STATE/deploy.20260101T000004Z.prev-dylib") == "dummy dylib A" ]] || fail "previous dylib not kept"
grep -q 'already exists in config.yaml' "$out" || fail "existing tierbridge config not noticed"
ok "--force replaces, keeps the previous file, notices an existing configs.tierbridge"

# the controller keeps editing after the deploy
printf '      priority: 99\n' >>"$home/config.yaml"
cp -p "$home/config.yaml" "$tmp/config.after"

# 6. rollback: dry-run and no --yes change nothing
if "$rollback" --dry-run >"$out" 2>&1; then :; else fail "rollback --dry-run failed"; fi
same "$home/config.yaml" "$tmp/config.after" && [[ $(cat "$target") == "dummy dylib B" ]] || fail "rollback --dry-run changed files"
if "$rollback" >"$out" 2>&1; then fail "rollback without --yes should not succeed"; fi
same "$home/config.yaml" "$tmp/config.after" && [[ $(cat "$target") == "dummy dylib B" ]] || fail "rollback without --yes changed files"
no_secret rollback-summary
grep -q 'line(s)' "$out" || fail "no diff summary printed"
ok "rollback --dry-run / missing --yes change nothing (count-only summary)"

# 7. rollback newest (m2): config back to the edited state, previous dylib put back
"$rollback" --yes >"$out" 2>&1 || fail "rollback newest failed"
no_secret rollback
same "$home/config.yaml" "$tmp/config.edited" || fail "config not restored by newest rollback"
[[ $(cat "$target") == "dummy dylib A" ]] || fail "previous dylib not put back"
[[ $(mode_of "$home/config.yaml") == 600 ]] || fail "restored config mode"
grep -q "controller's job" "$out" || fail "missing controller reminder"
ok "rollback (newest manifest) restores config and the replaced dylib"

# 8. rollback m1: original config, dylib removed
"$rollback" --manifest "$m1" --yes >"$out" 2>&1 || fail "rollback m1 failed"
same "$home/config.yaml" "$tmp/config.orig" || fail "config not restored to the original"
[[ ! -e $target ]] || fail "dylib not removed"
[[ -d $home/plugins/darwin/arm64 ]] || fail "rollback removed more than the dylib"
ok "rollback of the first deploy restores the original config and removes only the dylib"

# 9. changed dylib is left alone
run_deploy 20260101T000005Z || fail "deploy for tamper test failed"
printf 'tampered\n' >"$target"
"$rollback" --manifest "$TB_DEPLOY_STATE/deploy.20260101T000005Z.manifest" --yes >"$out" 2>&1 || fail "tamper rollback failed"
[[ $(cat "$target") == "tampered" ]] || fail "rollback removed a dylib whose sha256 changed"
grep -q 'LEFT IN PLACE' "$out" || fail "no warning about the changed dylib"
ok "rollback leaves a dylib whose sha256 no longer matches"

# 10. identical dylib already present: rollback must not remove it
printf 'dummy dylib B\n' >"$target"
run_deploy 20260101T000006Z || fail "deploy with identical dylib failed"
[[ $(jq -r 'select(.kind=="dylib")|.installed' "$TB_DEPLOY_STATE/deploy.20260101T000006Z.manifest") == false ]] || fail "installed should be false"
"$rollback" --yes >"$out" 2>&1 || fail "rollback of identical-dylib deploy failed"
[[ -f $target ]] || fail "rollback removed a dylib that deploy did not install"
ok "an already-identical dylib is never removed by rollback"

# 11. input validation
nb=$(find "$home" -maxdepth 1 -name 'config.yaml.bak.*' | wc -l | tr -d ' ')
if run_deploy 20260101T000007Z --alias 'bad alias;x'; then fail "bad alias accepted"; fi
if run_deploy 20260101T000007Z --max-attempts 0; then fail "bad max-attempts accepted"; fi
if run_deploy 20260101T000007Z --max-attempts 51; then fail "max-attempts 51 accepted"; fi
if run_deploy 20260101T000007Z --deadline-seconds -1; then fail "negative deadline accepted"; fi
nobak "$nb"
ok "invalid flags are rejected"

echo "self-test: $pass checks passed"
