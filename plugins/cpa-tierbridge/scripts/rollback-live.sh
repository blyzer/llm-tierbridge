#!/usr/bin/env bash
# rollback-live.sh - PREPARED, NEVER RUN BY THE AUTHOR against the real CPA instance.
#
# Undoes what deploy-live.sh staged, using its manifest:
#   * restores config.yaml from the recorded backup (cp -p), after printing a COUNT-ONLY diff summary
#     (config content is never printed) and only with --yes;
#   * removes ONLY the dylib the manifest says deploy installed, and only if its sha256 still matches;
#     if deploy replaced a previous file (--force) that copy is put back.
# It never restarts, reloads or signals anything: a restart/reload is the controller's job.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: rollback-live.sh [--manifest PATH] [--yes] [--dry-run]

  --manifest PATH   manifest written by deploy-live.sh
                    (default: newest ${TB_DEPLOY_STATE:-$HOME/.local/state/llm-tierbridge}/deploy.*.manifest)
  --yes             actually restore/remove; without it the summary is printed and nothing changes
  --dry-run         print the plan; touch nothing (also without --yes)
  -h, --help        this help
EOF
}

log() { printf '%s\n' "$*" >&2; }
die() { log "rollback-live: error: $*"; exit 1; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$1" | awk '{print $1}'
  else
    shasum -a 256 -- "$1" | awk '{print $1}'
  fi
}

manifest=""
yes=0
dry=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest) [[ $# -ge 2 ]] || die "--manifest needs a value"; manifest=$2; shift 2 ;;
    --yes) yes=1; shift ;;
    --dry-run) dry=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

command -v jq >/dev/null 2>&1 || die "jq is required"

if [[ -z $manifest ]]; then
  state_dir="${TB_DEPLOY_STATE:-$HOME/.local/state/llm-tierbridge}"
  [[ -d $state_dir ]] || die "state dir not found: $state_dir"
  # deploy.<UTC ts>.manifest sorts chronologically; the glob expands sorted
  for f in "$state_dir"/deploy.*.manifest; do
    [[ -f $f ]] && manifest=$f
  done
  [[ -n $manifest ]] || die "no deploy.*.manifest in $state_dir"
fi
[[ -f $manifest ]] || die "manifest not found: $manifest"
jq -e . "$manifest" >/dev/null 2>&1 || die "manifest is not valid JSON lines: $manifest"

mf() { # kind field -> value ("" when absent/null)
  jq -r --arg k "$1" --arg f "$2" \
    'select(.kind==$k and has($f)) | .[$f] | if .==null then "" else tostring end' "$manifest" | head -n 1
}
mfp() { # kind field subfield (object field)
  jq -r --arg k "$1" --arg f "$2" --arg s "$3" \
    'select(.kind==$k and (.[$f]|type)=="object") | .[$f][$s] // ""' "$manifest" | head -n 1
}

cfg=$(mf config path)
cfg_sha=$(mf config sha256)
bak=$(mf config backupPath)
bak_sha=$(mf config backupSha256)
d_target=$(mf dylib target)
d_sha=$(mf dylib sha256)
d_installed=$(mf dylib installed)
prev_path=$(mfp dylib previous path)
prev_sha=$(mfp dylib previous sha256)

# manifest sanity: never act outside the shapes deploy-live.sh produces
[[ -n $cfg && -n $bak && -n $bak_sha ]] || die "manifest has no config entry"
[[ $(basename -- "$cfg") == "config.yaml" ]] || die "manifest config path is not a config.yaml: $cfg"
case "$(basename -- "$bak")" in config.yaml.bak.*) ;; *) die "manifest backup name unexpected: $bak" ;; esac
[[ $(dirname -- "$bak") == "$(dirname -- "$cfg")" ]] || die "backup is not next to config.yaml"
if [[ -n $d_target ]]; then
  case "$(basename -- "$d_target")" in tierbridge-v*.dylib) ;; *) die "manifest dylib name unexpected: $d_target" ;; esac
  case "$d_target" in */darwin/arm64/tierbridge-v*.dylib) ;; *) die "manifest dylib is not under <dir>/darwin/arm64/: $d_target" ;; esac
fi

[[ -f $bak ]] || die "backup file is gone: $bak"
[[ $(sha256 "$bak") == "$bak_sha" ]] || die "backup sha256 changed since deploy; refusing to restore: $bak"
[[ -f $cfg ]] || die "live config.yaml not found: $cfg"

live_sha=$(sha256 "$cfg")
# count-only diff: lines the restore drops from the live file / brings back from the backup
counts=$(diff -- "$cfg" "$bak" | awk '/^</{a++} /^>/{b++} END{printf "%d %d", a+0, b+0}' || true)
drop=${counts% *}
back=${counts#* }

log "== rollback plan ($( [[ $dry -eq 1 ]] && echo DRY-RUN || echo EXECUTE )) from $manifest"
log "config.yaml     : $cfg"
if [[ $live_sha == "$bak_sha" ]]; then
  log "                  live file is already identical to the backup; restore is a no-op"
else
  if [[ $live_sha != "$cfg_sha" ]]; then
    log "                  live file differs from its state at deploy time (edited since)"
  fi
  log "                  restore drops $drop live line(s) and brings back $back backup line(s) (content not shown)"
fi
log "restore from    : $bak"

dylib_action="none"
if [[ $d_installed == "true" && -n $d_target ]]; then
  if [[ ! -e $d_target ]]; then
    log "dylib           : $d_target already absent; nothing to remove"
  elif [[ $(sha256 "$d_target") == "$d_sha" ]]; then
    dylib_action="remove"
    log "dylib           : remove $d_target (sha256 matches the one deploy installed)"
    if [[ -n $prev_path ]]; then
      if [[ -f $prev_path && $(sha256 "$prev_path") == "$prev_sha" ]]; then
        dylib_action="restore"
        log "                  and put back the replaced file from $prev_path"
      else
        log "                  WARNING: replaced file $prev_path missing or changed; it cannot be put back"
      fi
    fi
  else
    log "dylib           : $d_target changed since deploy (sha256 differs); LEFT IN PLACE"
  fi
else
  log "dylib           : deploy did not install anything (file was already identical); left in place"
fi

if [[ $dry -eq 1 ]]; then
  log "dry-run: nothing changed."
  exit 0
fi
if [[ $yes -ne 1 ]]; then
  log "nothing changed: re-run with --yes to apply."
  exit 2
fi

umask 077
if [[ $live_sha != "$bak_sha" ]]; then
  tmp="$cfg.rollback.$$"
  cp -p -- "$bak" "$tmp"
  mv -f -- "$tmp" "$cfg"
  [[ $(sha256 "$cfg") == "$bak_sha" ]] || die "restored config does not match the backup: $cfg"
  log "restored $cfg from backup."
fi
case "$dylib_action" in
  remove) rm -f -- "$d_target"; log "removed $d_target." ;;
  restore)
    tmp="$(dirname -- "$d_target")/.$(basename -- "$d_target").tmp.$$"
    cp -p -- "$prev_path" "$tmp"
    mv -f -- "$tmp" "$d_target"
    log "put back the previous $d_target."
    ;;
  *) ;;
esac
log "done. Restarting/reloading CPA is the controller's job; this script did not touch any process."
log "The backup, previous-dylib copy and manifest were kept in place."
