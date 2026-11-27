#!/usr/bin/env bash
# deploy-live.sh - PREPARED, NEVER RUN BY THE AUTHOR against the real CPA instance.
#
# Stages the tierbridge plugin into a CPA home and prints the config block the controller must add.
# What it does (and ONLY this):
#   1. backs up <CPA_HOME>/config.yaml to config.yaml.bak.<UTC ts> (cp -p, never overwrites);
#   2. copies the dylib to <plugins.dir>/darwin/arm64/tierbridge-v<semver>.dylib (never overwrites a
#      different file without --force; the previous file is kept in the state dir);
#   3. writes a JSON-lines manifest (no secrets) to ${TB_DEPLOY_STATE:-$HOME/.local/state/llm-tierbridge}/
#      deploy.<ts>.manifest, used by rollback-live.sh;
#   4. PRINTS the YAML block for plugins.configs plus instructions.
# It never edits config.yaml, never restarts, reloads or signals any process.
# --dry-run validates and prints the plan; it writes nothing at all.
#
# Host naming scheme (docs/cpa-plugin-smoke.md): <plugins.dir>/<GOOS>/<GOARCH>/<id>-v<semver>.dylib.
# Only the structural keys of the live `plugins:` block (dir, enabled, configs.tierbridge presence) are
# read from config.yaml; no other content of the file is read into the output.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)

usage() {
  cat <<'EOF'
usage: deploy-live.sh --router-file PATH [options]

  --dylib PATH              library to install (default: <plugin>/dist/tierbridge-v0.1.0.dylib);
                            the file name must be tierbridge-v<semver>.dylib
  --router-file PATH        tierbridge-router.json the plugin will read (required, must exist)
  --cpa-home DIR            CPA home (default: $CPA_HOME, else
                            $HOME/Library/Application Support/com.cpa.gui/cpa-core)
  --alias NAME              model alias the plugin answers to (default tierbridge/auto)
  --daily-budget-usd N      estimated daily spend cap, 0 = none (default 0)
  --cooldown-seconds N      cooldown after a failed upstream record (default 60)
  --max-attempts N          attempts per request, 1-50 (default 3)
  --deadline-seconds N      overall per-request deadline, 0 = none (default 120)
  --priority N              host priority of the plugin (default 10, see below)
  --force                   overwrite a DIFFERENT existing dylib of the same name
  --dry-run                 validate and print the plan; touch nothing
  -h, --help                this help

Environment: CPA_HOME, TB_DEPLOY_STATE (state/manifest dir, default $HOME/.local/state/llm-tierbridge).

Priority: the host orders its plugin chain by priority DESCENDING, then id ascending. Our router only
answers its own alias and otherwise falls through, so it must not outrank anything that serves other
traffic: the default is a deliberately low explicit number (10) instead of the host default.
EOF
}

log() { printf '%s\n' "$*" >&2; }
die() { log "deploy-live: error: $*"; exit 1; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | awk '{print $1}'
  else
    shasum -a 256 -- "$1" | awk '{print $1}'
  fi
}

abspath() { # existing file -> absolute physical dir + basename
  local d b
  d=$(cd -- "$(dirname -- "$1")" && pwd -P) || return 1
  b=$(basename -- "$1")
  printf '%s/%s' "$d" "$b"
}

yq() { # double-quoted YAML scalar
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  printf '"%s"' "$s"
}

no_ctl() { # name value
  if [[ $2 == *[[:cntrl:]]* ]]; then die "$1 contains control characters"; fi
}

dylib="$script_dir/../dist/tierbridge-v0.1.0.dylib"
router_file=""
cpa_home="${CPA_HOME:-$HOME/Library/Application Support/com.cpa.gui/cpa-core}"
alias_name="tierbridge/auto"
budget="0"
cooldown="60"
attempts="3"
deadline="120"
priority="10"
force=0
dry=0

need_val() { [[ $# -ge 2 ]] || die "$1 needs a value"; }
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dylib) need_val "$@"; dylib=$2; shift 2 ;;
    --router-file) need_val "$@"; router_file=$2; shift 2 ;;
    --cpa-home) need_val "$@"; cpa_home=$2; shift 2 ;;
    --alias) need_val "$@"; alias_name=$2; shift 2 ;;
    --daily-budget-usd) need_val "$@"; budget=$2; shift 2 ;;
    --cooldown-seconds) need_val "$@"; cooldown=$2; shift 2 ;;
    --max-attempts) need_val "$@"; attempts=$2; shift 2 ;;
    --deadline-seconds) need_val "$@"; deadline=$2; shift 2 ;;
    --priority) need_val "$@"; priority=$2; shift 2 ;;
    --force) force=1; shift ;;
    --dry-run) dry=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

# ---- validation (read-only) -------------------------------------------------------------------
command -v jq >/dev/null 2>&1 || die "jq is required (manifest is JSON lines)"
[[ -n $router_file ]] || { usage >&2; die "--router-file is required"; }
for pair in "dylib:$dylib" "router-file:$router_file" "cpa-home:$cpa_home" "alias:$alias_name"; do
  no_ctl "${pair%%:*}" "${pair#*:}"
done
[[ $alias_name =~ ^[A-Za-z0-9._/:-]+$ ]] || die "--alias may only contain letters, digits and . _ / : -"
[[ $budget =~ ^[0-9]+(\.[0-9]+)?$ ]] || die "--daily-budget-usd must be a non-negative number"
[[ $cooldown =~ ^[0-9]+$ ]] || die "--cooldown-seconds must be a non-negative integer"
[[ $attempts =~ ^[1-9][0-9]*$ && $attempts -le 50 ]] || die "--max-attempts must be an integer from 1 to 50"
[[ $deadline =~ ^[0-9]+$ ]] || die "--deadline-seconds must be a non-negative integer (0 = none)"
[[ $priority =~ ^-?[0-9]+$ ]] || die "--priority must be an integer"

[[ -f $dylib ]] || die "dylib not found: $dylib"
dylib=$(abspath "$dylib")
dylib_name=$(basename -- "$dylib")
semver_re='^tierbridge-v([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?)\.dylib$'
[[ $dylib_name =~ $semver_re ]] || die "dylib file name must be tierbridge-v<semver>.dylib, got: $dylib_name"
version=${BASH_REMATCH[1]}

[[ -f $router_file ]] || die "router file not found: $router_file"
router_file=$(abspath "$router_file")

[[ -d $cpa_home ]] || die "CPA home is not a directory: $cpa_home"
cpa_home=$(cd -- "$cpa_home" && pwd -P)
config="$cpa_home/config.yaml"
[[ -f $config ]] || die "no config.yaml in $cpa_home"

# Structural keys of the top-level `plugins:` block ONLY: dir, enabled, configs.tierbridge presence.
plugins_dir_raw=""
plugins_enabled=""
has_tierbridge=0
found_plugins=0
while IFS='=' read -r k v; do
  case "$k" in
    found) found_plugins=$v ;;
    dir) plugins_dir_raw=$v ;;
    enabled) plugins_enabled=$v ;;
    has_tierbridge) has_tierbridge=$v ;;
    *) ;;
  esac
done < <(awk '
  function val(s,   q) {
    sub(/^[A-Za-z-]+:[ \t]*/, "", s)
    if (s ~ /^"/) { sub(/^"/, "", s); sub(/".*$/, "", s); return s }
    if (s ~ /^\047/) { sub(/^\047/, "", s); sub(/\047.*$/, "", s); return s }
    sub(/[ \t]+#.*$/, "", s); sub(/[ \t]+$/, "", s)
    return s
  }
  BEGIN { inp = 0; base = -1; inc = 0; cbase = -1 }
  /^plugins:[ \t]*(#.*)?$/ { inp = 1; found = 1; next }
  inp && /^[^ \t#]/ { inp = 0 }
  !inp { next }
  /^[ \t]*(#|$)/ { next }
  {
    match($0, /^ */); ind = RLENGTH; line = substr($0, ind + 1)
    if (base < 0) base = ind
    if (ind == base) {
      inc = 0
      if (line ~ /^dir:/) d = val(line)
      else if (line ~ /^enabled:/) e = val(line)
      else if (line ~ /^configs:/) { inc = 1; cbase = -1 }
    } else if (ind > base && inc) {
      if (cbase < 0) cbase = ind
      if (ind == cbase && line ~ /^tierbridge:/) ht = 1
    }
  }
  END {
    print "found=" (found ? 1 : 0)
    print "dir=" d
    print "enabled=" e
    print "has_tierbridge=" (ht ? 1 : 0)
  }' "$config")

[[ $found_plugins == 1 ]] || die "config.yaml has no top-level plugins: block; refusing to guess the layout"
if [[ -z $plugins_dir_raw ]]; then
  log "deploy-live: warning: plugins.dir not set; assuming 'plugins' (relative to $cpa_home)"
  plugins_dir_raw="plugins"
fi
no_ctl "plugins.dir" "$plugins_dir_raw"
case "$plugins_dir_raw" in
  *..*) die "plugins.dir contains '..'; refusing: $plugins_dir_raw" ;;
  /*) plugins_dir=$plugins_dir_raw ;;
  *) plugins_dir="$cpa_home/$plugins_dir_raw" ;;
esac
target_dir="$plugins_dir/darwin/arm64"
target="$target_dir/$dylib_name"

ts=${TB_DEPLOY_TS:-$(date -u +%Y%m%dT%H%M%SZ)}
[[ $ts =~ ^[0-9]{8}T[0-9]{6}Z$ ]] || die "TB_DEPLOY_TS must look like 20260101T000000Z"
state_dir="${TB_DEPLOY_STATE:-$HOME/.local/state/llm-tierbridge}"
no_ctl "TB_DEPLOY_STATE" "$state_dir"
manifest="$state_dir/deploy.$ts.manifest"
backup="$config.bak.$ts"
prev_dylib="$state_dir/deploy.$ts.prev-dylib"
state_path="$state_dir/tierbridge-state.json"

[[ ! -e $backup ]] || die "backup already exists, refusing to overwrite: $backup"
[[ ! -e $manifest ]] || die "manifest already exists, refusing to overwrite: $manifest"

dylib_sha=$(sha256 "$dylib")
config_sha=$(sha256 "$config")
install_needed=1
replace_existing=0
if [[ -e $target || -L $target ]]; then
  [[ -f $target && ! -L $target ]] || die "target exists and is not a regular file: $target"
  if [[ $(sha256 "$target") == "$dylib_sha" ]]; then
    install_needed=0
  elif [[ $force -eq 1 ]]; then
    replace_existing=1
  else
    die "a different file already exists at $target; re-run with --force to replace it"
  fi
fi

# ---- plan -------------------------------------------------------------------------------------
log "== deploy-live plan ($( [[ $dry -eq 1 ]] && echo DRY-RUN || echo EXECUTE ))"
log "cpa home        : $cpa_home"
log "plugins.dir     : $plugins_dir_raw  (enabled: ${plugins_enabled:-unset})"
log "config backup   : $backup  (cp -p of config.yaml, sha256 $config_sha)"
if [[ $install_needed -eq 1 ]]; then
  log "dylib install   : $dylib -> $target  (sha256 $dylib_sha, version $version)"
  [[ $replace_existing -eq 1 ]] && log "                  replaces a different file (--force); previous copy kept at $prev_dylib"
else
  log "dylib install   : $target already identical (sha256 $dylib_sha); nothing to copy, rollback will leave it"
fi
log "manifest        : $manifest"
if [[ $has_tierbridge -eq 1 ]]; then
  log "note            : plugins.configs.tierbridge already exists in config.yaml; edit it instead of adding a second one"
fi
if [[ $plugins_enabled != "true" ]]; then
  log "note            : plugins.enabled is '${plugins_enabled:-unset}'; the controller must set it to true"
fi

if [[ $dry -eq 0 ]]; then
  # ---- execute --------------------------------------------------------------------------------
  umask 077
  mkdir -p -- "$state_dir"
  chmod 700 "$state_dir" 2>/dev/null || true

  cp -p -- "$config" "$backup"
  [[ $(sha256 "$backup") == "$config_sha" ]] || die "backup verification failed: $backup"

  previous_json="null"
  if [[ $replace_existing -eq 1 ]]; then
    cp -p -- "$target" "$prev_dylib"
    previous_json=$(jq -cn --arg p "$prev_dylib" --arg s "$(sha256 "$prev_dylib")" '{path:$p, sha256:$s}')
  fi

  {
    jq -cn --arg ts "$ts" --arg home "$cpa_home" \
      '{kind:"deploy", manifestVersion:1, ts:$ts, cpaHome:$home}'
    jq -cn --arg p "$config" --arg s "$config_sha" --arg b "$backup" \
      '{kind:"config", path:$p, sha256:$s, backupPath:$b, backupSha256:$s}'
    jq -cn --arg src "$dylib" --arg t "$target" --arg s "$dylib_sha" \
      --argjson inst "$([[ $install_needed -eq 1 ]] && echo true || echo false)" \
      --argjson prev "$previous_json" \
      '{kind:"dylib", source:$src, target:$t, sha256:$s, installed:$inst, previous:$prev}'
  } >"$manifest"

  if [[ $install_needed -eq 1 ]]; then
    mkdir -p -- "$target_dir"
    # temp name does not match <id>-v<semver>.dylib, so the host never discovers a half-written file
    tmp="$target_dir/.$dylib_name.tmp.$$"
    cp -- "$dylib" "$tmp"
    chmod 755 "$tmp"
    mv -f -- "$tmp" "$target"
    [[ $(sha256 "$target") == "$dylib_sha" ]] || die "installed dylib verification failed: $target"
  fi
  log "done: backup, dylib and manifest written. config.yaml was NOT modified."
fi

# ---- the block for the controller ---------------------------------------------------------------
cat <<EOF

# ---- add under plugins.configs in $config ----
# (indentation assumes plugins: > configs: > tierbridge: at 2/4 spaces; adjust to the file)
    tierbridge:
      enabled: true
      priority: $priority   # low on purpose: chain = priority desc, then id asc; we only answer the alias
      # tierbridge-router/2 host mode file, generated with: tierbridge export cpa-router --model-prefix '' (live ids)
      router-file: $(yq "$router_file")
      alias: $(yq "$alias_name")
      daily-budget-usd: $budget
      cooldown-seconds: $cooldown
      max-attempts: $attempts   # candidates tried per request (1-50)
      deadline-seconds: $deadline   # overall per-request deadline, all attempts; 0 = none
      decisions-keep: 50
      state-path: $(yq "$state_path")
# ---- end ----
EOF

cat >&2 <<EOF

Controller steps (this script does none of them):
  1. Edit $config:
     add the block above under plugins.configs (and make sure plugins.enabled is true).
  2. (Re)load/restart CPA yourself. The host re-evaluates plugins on a config reload; a new dylib
     version is picked up by the host's own rules. This script sends no signal and restarts nothing.
  3. Make sure the CPA process can write $state_dir (the plugin persists $state_path there).
  4. To undo: rollback-live.sh --manifest $manifest --yes   (then reload/restart CPA yourself).
EOF
