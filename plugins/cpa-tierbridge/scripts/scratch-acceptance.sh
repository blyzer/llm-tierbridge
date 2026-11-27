#!/usr/bin/env bash
# Scratch-CPA acceptance for the cpa-tierbridge plugin (slice 6; phase 1b: self executor with in-request fallback).
#
# Starts a THROWAWAY CLIProxyAPI on 127.0.0.1:$PORT (cwd, config, auth dir, plugins dir, router file
# and state file all inside one mktemp dir), loads ONLY our dylib, and sends at most MAX_REQS (12) tiny
# chat requests. It never touches the live CPA on :8317 (it is only the upstream the generated provider
# points at), never its config/plugins/process, and kills just the PID it started. The temp dir is
# removed on every exit path (trap).
#
# Env / args (all optional unless noted):
#   CATALOG    merged catalog.json            (default ~/.cache/llm-tierbridge/last/catalog.json)
#   GATEWAY    gateway source id              (default omp-live:easycliproxy)
#   NAME       provider name in the fragment  (default tbscratch)
#   PORT       scratch listener               (default 18417; never 8317)
#   CPA_BIN    CLIProxyAPI binary             (default the live binary path; only RUN with cwd = temp dir)
#   UPSTREAM   OpenAI-compatible base URL     (default http://127.0.0.1:8317/v1)
#   EASYCLIPROXY_API_KEY  upstream key (required; falls back to the macOS Keychain item
#              `security find-generic-password -s llm-tierbridge -a EASYCLIPROXY_API_KEY -w`)
#   DYLIB      prebuilt plugin dylib; default: build a private copy from this source tree into the
#              temp dir (never writes plugins/cpa-tierbridge/dist)
#   TB_BIN     tierbridge CLI; default target/debug/tierbridge, else `cargo run -q -p tierbridge-cli --`
#   ALIAS      alias the plugin answers       (default tierbridge/auto)
#   ALIAS_PREFIX                              (default tb-)
#   REQUIRE_MGMT=1  treat a 404 on /v0/management/tierbridge/* as FAIL (default: WARN, the route may
#              not be in the build under test)
#   PLUGIN_REF git ref to build the plugin from (e.g. HEAD) instead of the working tree
#   FAIL_UPSTREAM_ID  optional served id that the upstream fails today (429/5xx): adds the cooldown check
#              (the routed model errors upstream, the NEXT alias request must skip it; two more requests)
#   KILL_WAIT  seconds between the last request and SIGTERM in the persistence checks (default 0.2; use 1.8 to see
#              the debounced write alone, nothing pending at the kill)
#   VERBOSE=1  also print redacted log excerpts (plugin lines, host lines around the failure checks)
#
# Output: one compact PASS/FAIL/WARN/INFO line per check. Exit 1 if any FAIL. No secret is printed.
set -euo pipefail
umask 077

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
PLUGIN_SRC="$REPO/plugins/cpa-tierbridge"

CATALOG="${CATALOG:-$HOME/.cache/llm-tierbridge/last/catalog.json}"
GATEWAY="${GATEWAY:-omp-live:easycliproxy}"
NAME="${NAME:-tbscratch}"
PORT="${PORT:-18417}"
CPA_BIN="${CPA_BIN:-$HOME/Library/Application Support/com.cpa.gui/cpa-core/cli-proxy-api}"
UPSTREAM="${UPSTREAM:-http://127.0.0.1:8317/v1}"
ALIAS="${ALIAS:-tierbridge/auto}"
ALIAS_PREFIX="${ALIAS_PREFIX:-tb-}"
REQUIRE_MGMT="${REQUIRE_MGMT:-0}"
VERBOSE="${VERBOSE:-0}"
MAX_REQS=12
MAX_PROBES=8

TMP=""
PID=""
REQS=0
FAILS=0
LIVE_LISTENER_BEFORE=""

die() { echo "ERROR: $*" >&2; exit 2; }
pass() { printf 'PASS %-16s %s\n' "$1" "${2:-}"; }
fail() { printf 'FAIL %-16s %s\n' "$1" "${2:-}"; FAILS=$((FAILS + 1)); }
warn() { printf 'WARN %-16s %s\n' "$1" "${2:-}"; }
info() { printf 'INFO %-16s %s\n' "$1" "${2:-}"; }

# redact: replace the three secrets (and anything that looks like an sk- key) in stdin.
redact() {
  R1="${EASYCLIPROXY_API_KEY:-}" R2="${CLIENT_KEY:-}" R3="${MGMT_SECRET:-}" awk '
    function rep(s, k, tag,   out, i) {
      if (k == "") return s
      out = ""
      while ((i = index(s, k)) > 0) { out = out substr(s, 1, i - 1) tag; s = substr(s, i + length(k)) }
      return out s
    }
    { l = rep($0, ENVIRON["R1"], "<upstream-key>"); l = rep(l, ENVIRON["R2"], "<client-key>")
      l = rep(l, ENVIRON["R3"], "<mgmt-secret>"); gsub(/sk-[A-Za-z0-9_-]+/, "<sk-redacted>", l)
      gsub(/API key [^ ]+/, "API key <masked>", l); gsub(/api_key=[^ ]+/, "api_key=<masked>", l); print l }'
}

# curl with an Authorization header passed through stdin (keeps the token out of ps).
curl_auth() { # token curl-args...
  local tok="$1"; shift
  printf 'header = "Authorization: Bearer %s"\n' "$tok" | curl -sS -m 90 -K - "$@"
}

stop_cpa() {
  [ -n "$PID" ] || return 0
  if kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null || true
    for _ in $(seq 1 40); do kill -0 "$PID" 2>/dev/null || break; sleep 0.25; done
    kill -0 "$PID" 2>/dev/null && kill -9 "$PID" 2>/dev/null || true
  fi
  wait "$PID" 2>/dev/null || true
  PID=""
}

cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  stop_cpa
  if [ -n "$TMP" ] && [[ "$TMP" == /tmp/tb-scratch.* ]]; then rm -rf "$TMP"; fi
  # Verify: nothing of ours left on the scratch port, the live listener untouched.
  if [ -n "$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null || true)" ]; then
    echo "FAIL cleanup          something still listens on :$PORT" >&2; rc=1
  else
    echo "INFO cleanup          scratch process stopped, :$PORT free, temp dir removed (requests sent: $REQS/$MAX_REQS)"
  fi
  local live_after
  live_after="$(lsof -nP -iTCP:8317 -sTCP:LISTEN -t 2>/dev/null | head -n 1 || true)"
  if [ "$live_after" = "$LIVE_LISTENER_BEFORE" ]; then
    echo "INFO cleanup          live :8317 listener unchanged (pid ${live_after:-none})"
  else
    echo "FAIL cleanup          live :8317 listener changed: ${LIVE_LISTENER_BEFORE:-none} -> ${live_after:-none}" >&2; rc=1
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

tb() {
  if [ -n "${TB_BIN:-}" ]; then "$TB_BIN" "$@"
  elif [ -x "$REPO/target/debug/tierbridge" ]; then "$REPO/target/debug/tierbridge" "$@"
  else (cd "$REPO" && cargo run -q -p tierbridge-cli -- "$@"); fi
}

# ---------- preflight ----------
[ "$PORT" != "8317" ] || die "PORT must not be the live CPA port 8317"
for c in curl jq awk lsof go; do command -v "$c" >/dev/null || die "missing tool: $c"; done
[ -x "$CPA_BIN" ] || die "CPA_BIN not executable: $CPA_BIN"
[ -f "$CATALOG" ] || die "CATALOG not found: $CATALOG (build one with 'tierbridge catalog build --merge', or point CATALOG at a public-fixture catalog)"
if [ -n "$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null || true)" ]; then die "something already listens on :$PORT"; fi
if [ -z "${EASYCLIPROXY_API_KEY:-}" ] && command -v security >/dev/null; then
  EASYCLIPROXY_API_KEY="$(security find-generic-password -s llm-tierbridge -a EASYCLIPROXY_API_KEY -w 2>/dev/null || true)"
fi
[ -n "${EASYCLIPROXY_API_KEY:-}" ] || die "EASYCLIPROXY_API_KEY is not set and not in the Keychain (service llm-tierbridge, account EASYCLIPROXY_API_KEY)"
export EASYCLIPROXY_API_KEY
LIVE_LISTENER_BEFORE="$(lsof -nP -iTCP:8317 -sTCP:LISTEN -t 2>/dev/null | head -n 1 || true)"

TMP="$(mktemp -d /tmp/tb-scratch.XXXXXX)"
mkdir -p "$TMP/plugins/darwin/arm64" "$TMP/auth" "$TMP/build"
CLIENT_KEY="tbacc-$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
MGMT_SECRET="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
UP_BASE="${UPSTREAM%/}"

# ---------- plugin dylib (private copy; the shared dist/ is never touched) ----------
DYLIB_DST="$TMP/plugins/darwin/arm64/tierbridge-v0.1.0.dylib"
if [ -n "${DYLIB:-}" ]; then
  [ -f "$DYLIB" ] || die "DYLIB not found: $DYLIB"
  cp "$DYLIB" "$DYLIB_DST"
else
  SRC_DIR="$PLUGIN_SRC"
  if [ -n "${PLUGIN_REF:-}" ]; then   # build a committed snapshot, immune to a half-edited working tree
    mkdir -p "$TMP/src"
    git -C "$REPO" archive "$PLUGIN_REF" plugins/cpa-tierbridge | tar -x -C "$TMP/src"
    SRC_DIR="$TMP/src/plugins/cpa-tierbridge"
  fi
  (cd "$SRC_DIR" && GOARCH=arm64 GOARM64=v8.0 CGO_ENABLED=1 CGO_CFLAGS=-march=armv8-a \
    go build -buildmode=c-shared -o "$TMP/build/tierbridge.dylib" .) || die "go build failed"
  mv "$TMP/build/tierbridge.dylib" "$DYLIB_DST"
fi
[ "$(find "$TMP/plugins" -type f | wc -l | tr -d ' ')" = "1" ] || die "plugins dir must hold exactly one file"

# ---------- provider fragment + router (repo CLI) ----------
tb export cpa --catalog "$CATALOG" --gateway "$GATEWAY" --base-url "$UPSTREAM" --name "$NAME" \
  --alias-prefix "$ALIAS_PREFIX" --api-key-env EASYCLIPROXY_API_KEY \
  --out "$TMP/provider.yaml" >/dev/null 2>"$TMP/export.err" || { redact <"$TMP/export.err" >&2; die "export cpa failed"; }
tb export cpa-router --catalog "$CATALOG" --gateway "$GATEWAY" --model-prefix "$ALIAS_PREFIX" \
  --alias "$ALIAS" --out "$TMP/router.full.json" >/dev/null 2>"$TMP/export.err" || { redact <"$TMP/export.err" >&2; die "export cpa-router failed"; }

# Which ids does the upstream really serve? (count + a few only)
curl_auth "$EASYCLIPROXY_API_KEY" -o "$TMP/upstream-models.json" "$UP_BASE/models" >/dev/null || die "upstream /models unreachable"
jq -r '.data[].id' "$TMP/upstream-models.json" | sort -u >"$TMP/live-ids.txt"
info upstream "serves $(wc -l <"$TMP/live-ids.txt" | tr -d ' ') ids; e.g. $(head -n 3 "$TMP/live-ids.txt" | paste -sd, -)"

# Pick up to 3 free, served candidates that really answer (<= MAX_PROBES direct upstream probes).
SEL="$TMP/selected.ndjson"; : >"$SEL"
probes=0
while IFS= read -r cand; do
  id="$(jq -r '.id' <<<"$cand")"
  grep -qxF "$id" "$TMP/live-ids.txt" || continue
  [ "$probes" -lt "$MAX_PROBES" ] || break
  probes=$((probes + 1))
  code="$(curl_auth "$EASYCLIPROXY_API_KEY" -o "$TMP/probe.json" -w '%{http_code}' -H 'Content-Type: application/json' \
    "$UP_BASE/chat/completions" -d "$(jq -nc --arg m "$id" '{model:$m,max_tokens:16,messages:[{role:"user",content:"Say OK"}]}')" || echo 000)"
  if [ "$code" = 200 ] && jq -e '.choices|length>0' "$TMP/probe.json" >/dev/null 2>&1; then
    echo "$cand" >>"$SEL"
    [ "$(wc -l <"$SEL" | tr -d ' ')" -ge 3 ] && break
  fi
done < <(jq -c --arg a "$ALIAS" '[.aliases[]|select(.alias==$a)|.candidates[]|select(.cost.status=="free")|select(.contextWindow==null or .contextWindow>=16000)]|.[0:60][]' "$TMP/router.full.json")
[ "$(wc -l <"$SEL" | tr -d ' ')" -ge 2 ] || die "fewer than 2 answering free candidates found ($probes probes)"
CAND_A="$(sed -n 1p "$SEL")"; CAND_B="$(sed -n 2p "$SEL")"
ID_A="$(jq -r '.id' <<<"$CAND_A")"; MODEL_A="$(jq -r '.model' <<<"$CAND_A")"
ID_B="$(jq -r '.id' <<<"$CAND_B")"; MODEL_B="$(jq -r '.model' <<<"$CAND_B")"
info candidates "A=$ID_A (rpm 1)  B=$ID_B  ($probes upstream probes)"

# write_router FILE JSON-ARRAY-OF-CANDIDATES: the full router file with the alias' candidates replaced.
write_router() {
  jq --arg a "$ALIAS" --argjson c "$2" '.aliases |= map(if .alias==$a then .candidates=$c else . end)' \
    "$TMP/router.full.json" >"$1.new"
  mv "$1.new" "$1"
}
sel_json() { jq -sc '.' "$SEL"; }
GOOD_ROUTER="$(jq -c --argjson first "$(jq -c '.quota={rpm:1,rpd:null}' <<<"$CAND_A")" '[$first] + .[1:]' <<<"$(sel_json)")"
write_router "$TMP/router.json" "$GOOD_ROUTER"

# ---------- scratch config ----------
write_config() { # debug-force-error true|false
  {
    cat <<EOF
config-version: 8
server:
  host: "127.0.0.1"
  port: $PORT
management:
  allow-remote: false
  secret-key: "$MGMT_SECRET"
  disable-control-panel: true
access:
  api-keys:
    - "$CLIENT_KEY"
oauth:
  auth-dir: "$TMP/auth"
observability:
  logs:
    debug: true
    logging-to-file: false
  usage:
    usage-statistics-enabled: true
plugins:
  enabled: true
  dir: "$TMP/plugins"
  configs:
    tierbridge:
      enabled: true
      priority: 100
      router-file: "$TMP/router.json"
      alias: "$ALIAS"
      daily-budget-usd: 1
      cooldown-seconds: 30
      state-path: "$TMP/state.json"
      decisions-keep: 50
      debug-force-error: $1
EOF
    cat "$TMP/provider.yaml"   # api-keys: { openai-compatibility: [...] } (config-version 8 layout)
  } >"$TMP/config.yaml"
  chmod 600 "$TMP/config.yaml"
}

start_cpa() {
  : >"$TMP/cpa.out"
  ( cd "$TMP" && exec "$CPA_BIN" -config "$TMP/config.yaml" ) >>"$TMP/cpa.out" 2>&1 &
  PID=$!
  local code
  for _ in $(seq 1 80); do
    kill -0 "$PID" 2>/dev/null || { redact <"$TMP/cpa.out" | tail -n 15 >&2; die "scratch CPA exited during startup"; }
    code="$(curl_auth "$CLIENT_KEY" -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/v1/models" 2>/dev/null || true)"
    [ "$code" = 200 ] && return 0
    sleep 0.5
  done
  redact <"$TMP/cpa.out" | tail -n 15 >&2; die "scratch CPA did not become ready"
}

# chat MODEL -> CODE, BODY (file), RMODEL (model the upstream answered with)
chat() {
  REQS=$((REQS + 1))
  [ "$REQS" -le "$MAX_REQS" ] || die "request budget ($MAX_REQS) exceeded"
  BODY="$TMP/resp.$REQS.json"
  CODE="$(curl_auth "$CLIENT_KEY" -o "$BODY" -w '%{http_code}' -H 'Content-Type: application/json' \
    "http://127.0.0.1:$PORT/v1/chat/completions" \
    -d "$(jq -nc --arg m "$1" '{model:$m,max_tokens:16,messages:[{role:"user",content:"Say OK"}]}')" 2>/dev/null || echo 000)"
  RMODEL="$(jq -r '.model // ""' "$BODY" 2>/dev/null || true)"
}
errsum() { jq -r '[.error.code // "", .error.type // "", (.error.message // "" | .[0:90])] | map(select(. != "")) | join(" | ")' "$BODY" 2>/dev/null | redact || true; }

# mgmt PATH [auth|noauth] -> MCODE, MBODY (file)
mgmt() {
  MBODY="$TMP/mgmt.out"
  if [ "${2:-auth}" = auth ]; then
    MCODE="$(curl_auth "$MGMT_SECRET" -o "$MBODY" -w '%{http_code}' "http://127.0.0.1:$PORT$1" 2>/dev/null || echo 000)"
  else
    MCODE="$(curl -sS -m 30 -o "$MBODY" -w '%{http_code}' "http://127.0.0.1:$PORT$1" 2>/dev/null || echo 000)"
  fi
}

logmark() { wc -c <"$TMP/cpa.out" | tr -d ' '; }
logsince() { tail -c +"$(($1 + 1))" "$TMP/cpa.out" | redact; }
excerpt() { # offset pattern max
  [ "$VERBOSE" = 1 ] || return 0
  logsince "$1" | awk -v p="$2" -v n="$3" '$0 ~ p && c++ < n { print "      | " substr($0, 1, 260) }'
}

# chat_stream MODEL -> CODE, BODY (SSE text), NCHUNK (data: lines), DONE (1 when [DONE] seen)
chat_stream() {
  REQS=$((REQS + 1))
  [ "$REQS" -le "$MAX_REQS" ] || die "request budget ($MAX_REQS) exceeded"
  BODY="$TMP/resp.$REQS.sse"
  CODE="$(curl_auth "$CLIENT_KEY" -N -o "$BODY" -w '%{http_code}' -H 'Content-Type: application/json' \
    "http://127.0.0.1:$PORT/v1/chat/completions" \
    -d "$(jq -nc --arg m "$1" '{model:$m,stream:true,max_tokens:16,messages:[{role:"user",content:"Say OK"}]}')" 2>/dev/null || echo 000)"
  NCHUNK="$(grep -c '^data:' "$BODY" 2>/dev/null || true)"; NCHUNK="${NCHUNK:-0}"
  DONE=0; grep -q '^data: *\[DONE\]' "$BODY" 2>/dev/null && DONE=1
}
# last_decision JQ-FILTER -> prints the filter applied to the newest /decisions entry
last_decision() { mgmt "/v0/management/tierbridge/decisions" auth; jq -c ".decisions[0] | $1" "$MBODY" 2>/dev/null | redact | cut -c1-420; }
attempts_of() { last_decision '[.attempts[]? | {rank,model,status,ok,fallback,coolingDown,error:(.error // "" | .[0:70])}]'; }

need_mgmt() { # endpoint check-name: fetch it into MBODY; on non-200 FAIL (REQUIRE_MGMT=1) or WARN and return 1
  mgmt "/v0/management/tierbridge/$1" auth
  [ "$MCODE" = 200 ] && return 0
  if [ "$REQUIRE_MGMT" = 1 ]; then fail "$2" "GET $1 -> $MCODE"; else warn "$2" "GET $1 -> $MCODE (route not in this build)"; fi
  return 1
}
FAIL_MODEL="${ALIAS_PREFIX}${FAIL_UPSTREAM_ID:-}"
BAD_MODEL="${ALIAS_PREFIX}does-not-exist"
BAD_CAND="$(jq -c --arg m "$BAD_MODEL" '.model=$m | .id="does-not-exist" | .quota=null' <<<"$CAND_B")"
if [ -z "${STREAM:-}" ]; then   # streaming is tested once the plugin has a streaming executor
  if git -C "$REPO" log --oneline "${PLUGIN_REF:-HEAD}" 2>/dev/null | grep -qi 'streaming executor'; then STREAM=1; else STREAM=0; fi
fi

# ================= phase 1: plugin as configured =================
write_config false
start_cpa

# (1) load / register / capabilities
if grep -q 'pluginhost: plugin registered' "$TMP/cpa.out"; then
  pass plugin-registered "$(grep -m1 'pluginhost: plugin registered' "$TMP/cpa.out" | redact | grep -oE 'plugin_(name|id|version)=[^ ]+' | paste -sd' ' - | cut -c1-120)"
else
  fail plugin-registered "no 'pluginhost: plugin registered' line"
fi
[ "$VERBOSE" != 1 ] || awk '/pluginhost/ { print "      | " substr($0, 1, 300) }' "$TMP/cpa.out" | redact
mgmt /v0/management/plugins auth
if [ "$MCODE" = 200 ]; then
  info plugin-caps "$(jq -c '[.. | objects | select(.id? == "tierbridge" or .name? == "tierbridge")] | max_by(length) | {registered, effective_enabled, supports_oauth, supports_quota, keys: keys}' "$MBODY" 2>/dev/null | cut -c1-400 | redact)"
else
  info plugin-caps "GET /v0/management/plugins -> $MCODE"
fi

# (2) alias served by the self executor: 200, decisions show attempts[ok]
M=$(logmark)
chat "$ALIAS"
if [ "$CODE" = 200 ] && jq -e '.choices|length>0' "$BODY" >/dev/null 2>&1; then
  att="$(attempts_of)"
  if jq -e 'length==1 and .[0].ok==true and .[0].status==200 and .[0].model=="'"$MODEL_A"'"' <<<"$att" >/dev/null 2>&1; then
    pass route-self "200 via self executor; attempts $att; client saw model=${RMODEL:-?}"
  else
    fail route-self "200 but attempts=$att (want one ok attempt on $MODEL_A)"
  fi
else
  fail route-self "http $CODE $(errsum)"
fi
excerpt "$M" 'tb-|executor|route|model' 6
# rpm-1 candidate exhausted: the next alias request starts at the next candidate
chat "$ALIAS"; C2="$CODE"
mgmt /v0/management/tierbridge/decisions auth
if [ "$C2" = 200 ] && jq -e --arg m "$MODEL_B" --arg a "$MODEL_A" '.decisions[0] | .chosen.model == $m and ([.skipped[]? | select(.model == $a)] | length > 0)' "$MBODY" >/dev/null 2>&1; then
  pass rpm-skip "2nd alias request skipped $MODEL_A (rpm) -> $MODEL_B; skipped $(jq -c '[.decisions[0].skipped[]? | {model,reason}]' "$MBODY" | cut -c1-200)"
else
  fail rpm-skip "http $C2 decision=$(jq -c '.decisions[0]|{chosen,reason,skipped}' "$MBODY" 2>/dev/null | cut -c1-300)"
fi
# (5) non-alias model is unaffected
chat "$MODEL_A"
if [ "$CODE" = 200 ] && jq -e '.choices|length>0' "$BODY" >/dev/null 2>&1; then pass non-alias "$MODEL_A -> 200 (plugin not involved), model=${RMODEL:-?}"; else fail non-alias "http $CODE $(errsum)"; fi

# (8) management endpoints (answer with the secret, 401 without)
mgmt /v0/management/config auth; base_with="$MCODE"
mgmt /v0/management/config noauth; base_without="$MCODE"
if [ "$base_with" = 200 ] && [ "$base_without" = 401 ]; then pass mgmt-baseline "host /config: 200 with secret, 401 without"; else fail mgmt-baseline "host /config: with=$base_with without=$base_without"; fi
for ep in status candidates counters budget decisions; do
  mgmt "/v0/management/tierbridge/$ep" auth; w="$MCODE"
  mgmt "/v0/management/tierbridge/$ep" noauth; n="$MCODE"
  if [ "$w" = 200 ] && [ "$n" = 401 ]; then
    pass "mgmt-$ep" "200 with secret, 401 without"
  elif [ "$w" = 404 ] && [ "$REQUIRE_MGMT" != 1 ]; then
    warn "mgmt-$ep" "404 (route not in this build); without secret: $n"
  else
    fail "mgmt-$ep" "with=$w without=$n"
  fi
done
if [ "$VERBOSE" = 1 ]; then
  for ep in status candidates counters budget decisions; do
    mgmt "/v0/management/tierbridge/$ep" auth
    echo "      | [after-requests] $ep -> $MCODE $(jq -c . "$MBODY" 2>/dev/null | redact | cut -c1-1500)"
  done
fi

# (7) usage records exactly as the host sends them (inner executions of the self executor)
if need_mgmt status usage-records; then
  info usage-records "$(jq -c '[.lastUsageModels[]? | {model,alias,provider,failed,status,matched}] | unique_by([.model,.alias,.provider,.failed,.matched])' "$MBODY" | redact | cut -c1-900)"
  if jq -e --arg a "$MODEL_B" --arg i "$ID_B" '[.lastUsageModels[]? | select((.model == $a or .model == $i) and .matched == true)] | length > 0' "$MBODY" >/dev/null 2>&1; then
    pass usage-match "a matched usage record carries the candidate model ($MODEL_B or $ID_B)"
  else
    fail usage-match "no matched record with model $MODEL_B/$ID_B"
  fi
fi
# counters of the chosen model
if need_mgmt counters counters-used; then
  row="$(jq -c --arg m "$MODEL_A" '[.models[]? | select(.model == $m)][0] // {} | {rpm,rpd,tpm,tpd}' "$MBODY")"
  if jq -e '.rpm.used >= 1 and .rpm.limit == 1 and .rpd.used >= 1 and .tpm.used > 0 and .tpd.used > 0' <<<"$row" >/dev/null 2>&1; then
    pass counters-used "$MODEL_A $(jq -c '{rpm:.rpm.used,rpmLimit:.rpm.limit,rpd:.rpd.used,tpm:.tpm.used,tpd:.tpd.used}' <<<"$row")"
  else
    fail counters-used "$MODEL_A row=$row"
  fi
fi
if need_mgmt budget budget; then
  if jq -e '.dailyBudgetUsd == 1 and (.spentUsd | type) == "number" and .remainingUsd <= 1' "$MBODY" >/dev/null 2>&1; then
    pass budget "$(jq -c '{day,dailyBudgetUsd,spentUsd,remainingUsd,unpriced}' "$MBODY" | cut -c1-200)"
  else
    fail budget "$(jq -c . "$MBODY" | cut -c1-200)"
  fi
fi

# (3) IN-REQUEST fallback: a nonexistent model first, real ones behind it; ONE client call must succeed
write_router "$TMP/router.json" "$(jq -c --argjson bad "$BAD_CAND" '[$bad] + .' <<<"$(sel_json)")"
M=$(logmark)
chat "$ALIAS"
if [ "$CODE" = 200 ] && jq -e '.choices|length>0' "$BODY" >/dev/null 2>&1; then
  att="$(attempts_of)"
  if jq -e --arg b "$BAD_MODEL" '.[0].model == $b and .[0].ok == false and .[0].fallback == true and .[1].ok == true' <<<"$att" >/dev/null 2>&1; then
    pass fallback "1 client call: attempt1 $BAD_MODEL failed -> attempt2 ok; $att"
  else
    fail fallback "200 but attempts=$att"
  fi
else
  fail fallback "http $CODE $(errsum); attempts=$(attempts_of)"
fi
excerpt "$M" 'does-not-exist|auth_not_found|executor|retry' 8

# (4) cooldown: a candidate the upstream really fails (FAIL_UPSTREAM_ID, e.g. 429). In-request fallback answers the
# client; the NEXT alias request must skip the failed candidate with reason cooldown.
if [ -n "${FAIL_UPSTREAM_ID:-}" ]; then
  write_router "$TMP/router.json" "$(jq -c --arg id "$FAIL_UPSTREAM_ID" --arg m "$FAIL_MODEL" --argjson bad "$BAD_CAND" '[($bad | .model=$m | .id=$id)] + .' <<<"$(sel_json)")"
  chat "$ALIAS"
  att="$(attempts_of)"
  info fail-upstream "top=$FAIL_MODEL -> http $CODE (client); attempts $att"
  if [ "$CODE" = 200 ] && jq -e --arg f "$FAIL_MODEL" '.[0].model == $f and .[0].ok == false and .[0].fallback == true and .[1].ok == true' <<<"$att" >/dev/null 2>&1; then
    pass fallback-429 "upstream failure on $FAIL_MODEL fell back in the same request: $att"
  else
    fail fallback-429 "http $CODE attempts=$att"
  fi
  for _ in $(seq 1 12); do
    mgmt "/v0/management/tierbridge/counters" auth
    jq -e --arg m "$FAIL_MODEL" '[.models[]? | select(.model == $m and .cooldownRemainingSeconds > 0)] | length > 0' "$MBODY" >/dev/null 2>&1 && break
    sleep 0.5
  done
  M=$(logmark)
  chat "$ALIAS"
  mgmt "/v0/management/tierbridge/decisions" auth
  if [ "$CODE" = 200 ] && jq -e --arg f "$FAIL_MODEL" '.decisions[0] | ([.skipped[]? | select(.model == $f and .reason == "cooldown")] | length > 0) and ([.attempts[]? | select(.model == $f)] | length == 0)' "$MBODY" >/dev/null 2>&1; then
    pass cooldown "next alias request skipped $FAIL_MODEL (cooldown), no attempt on it; $(jq -c '.decisions[0] | {chosen,reason}' "$MBODY" | cut -c1-200)"
  else
    fail cooldown "http $CODE decisions[0]=$(jq -c '.decisions[0] | {chosen,reason,skipped}' "$MBODY" 2>/dev/null | cut -c1-300)"
  fi
  mgmt "/v0/management/tierbridge/counters" auth
  info cooldown "counters: $(jq -c --arg m "$FAIL_MODEL" '[.models[]? | select(.model == $m)][0] | {model, rpd, cooldownRemainingSeconds}' "$MBODY" 2>/dev/null | cut -c1-240)"
else
  warn cooldown "skipped: set FAIL_UPSTREAM_ID to a served id the upstream fails today"
fi

# (10) streaming (needs the streaming executor)
if [ "$STREAM" = 1 ]; then
  sse_head() { head -c 160 "$BODY" | redact | tr '\n' ' '; }
  write_router "$TMP/router.json" "$GOOD_ROUTER"
  chat_stream "$ALIAS"
  if [ "$CODE" = 200 ] && [ "$NCHUNK" -ge 2 ] && [ "$DONE" = 1 ]; then
    pass stream "stream:true alias -> 200, $NCHUNK SSE data lines, [DONE]; winning $(attempts_of); first bytes: $(sse_head)"
  else
    fail stream "http $CODE chunks=$NCHUNK done=$DONE $(sse_head)"
  fi
  # a different nonexistent model: the one used by the non-stream fallback check is on cooldown by now and would be skipped
  BAD_CAND_S="$(jq -c --arg m "${ALIAS_PREFIX}does-not-exist-stream" '.model=$m | .id="does-not-exist-stream"' <<<"$BAD_CAND")"
  write_router "$TMP/router.json" "$(jq -c --argjson bad "$BAD_CAND_S" '[$bad] + .' <<<"$(sel_json)")"
  chat_stream "$ALIAS"
  att="$(attempts_of)"
  if [ "$CODE" = 200 ] && [ "$NCHUNK" -ge 2 ] && [ "$DONE" = 1 ] && jq -e '.[0].ok == false and .[0].fallback == true and .[1].ok == true' <<<"$att" >/dev/null 2>&1; then
    pass stream-fallback "nonexistent first: fell back before the first chunk, client saw a normal SSE stream ($NCHUNK chunks); $att; first bytes: $(sse_head)"
  else
    fail stream-fallback "http $CODE chunks=$NCHUNK done=$DONE attempts=$att first bytes: $(sse_head)"
  fi
  chat_stream "$MODEL_B"
  if [ "$CODE" = 200 ] && [ "$NCHUNK" -ge 2 ] && [ "$DONE" = 1 ]; then pass stream-non-alias "$MODEL_B stream:true -> 200, $NCHUNK chunks (plugin not involved)"; else fail stream-non-alias "http $CODE chunks=$NCHUNK done=$DONE $(sse_head)"; fi
else
  warn stream "pending: no 'streaming executor' commit in ${PLUGIN_REF:-HEAD} (set STREAM=1 to force)"
fi
write_router "$TMP/router.json" "$GOOD_ROUTER"
sleep "${KILL_WAIT:-0.2}"   # seconds between the last request and the live snapshot + SIGTERM (the state flush is debounced ~1 s)
mgmt "/v0/management/tierbridge/counters" auth
LIVE_TPD_A="$(jq -r --arg m "$MODEL_A" '[.models[]? | select(.model == $m)][0].tpd.used // 0' "$MBODY" 2>/dev/null || echo 0)"
mgmt "/v0/management/tierbridge/status" auth
LIVE_PERSIST="$(jq -c '.persistence | {dirty, saves, lastSaveAt}' "$MBODY" 2>/dev/null || echo '{}')"
stop_cpa

# (9) persistence: the state file right after SIGTERM
ST="$TMP/state.json"
state_mode() { stat -f '%Lp' "$ST" 2>/dev/null || stat -c '%a' "$ST"; }
if [ -f "$ST" ]; then
  mode="$(state_mode)"
  if [ "$mode" = 600 ] && jq -e --arg m "$MODEL_A" \
    '.version == 1 and ([.models | to_entries[] | select(.key | endswith($m)) | .value | select(.rpd >= 1)] | length > 0)' "$ST" >/dev/null 2>&1; then
    pass persist-write "state.json mode $mode, $(jq -c '{version,day,models:(.models|length)}' "$ST")"
  else
    fail persist-write "mode=$mode content: $(jq -c '{version,day,keys:(.models|keys)}' "$ST" 2>/dev/null | cut -c1-300)"
  fi
  missing=""
  file_tpd="$(jq -r --arg m "$MODEL_A" '[.models | to_entries[] | select(.key | endswith($m)) | .value.tpd] | first // 0' "$ST" 2>/dev/null || echo 0)"
  [ "$file_tpd" -ge "$LIVE_TPD_A" ] 2>/dev/null || missing="$missing tpd($MODEL_A: file $file_tpd, live $LIVE_TPD_A)"
  info persist-flush "just before SIGTERM (live status.persistence) $LIVE_PERSIST; file savedAt=$(jq -r '.savedAt' "$ST" 2>/dev/null)"
  jq -e --arg m "$MODEL_B" '[.models | to_entries[] | select(.key | endswith($m))] | length > 0' "$ST" >/dev/null 2>&1 || missing="$missing model($MODEL_B)"
  if [ -n "${FAIL_UPSTREAM_ID:-}" ]; then
    jq -e --arg m "$FAIL_MODEL" '[.models | to_entries[] | select(.key | endswith($m)) | .value | select(.cooldownUntil != null and .cooldownUntil != "0001-01-01T00:00:00Z")] | length > 0' "$ST" >/dev/null 2>&1 || missing="$missing cooldown($FAIL_MODEL)"
  fi
  if [ -z "$missing" ]; then
    pass persist-flush "state at SIGTERM holds the whole run (tpd, 2nd model, cooldown)"
  else
    fail persist-flush "state at SIGTERM lacks:$missing"
  fi
else
  fail persist-write "no $ST after shutdown"
fi

# ================= phase 2: restart (state reload) + debug-force-error =================
write_router "$TMP/router.json" "$GOOD_ROUTER"
write_config true
start_cpa
if need_mgmt status persist-reload; then
  if jq -e '.persistence.loaded == true' "$MBODY" >/dev/null 2>&1; then
    pass persist-reload "$(jq -c '.persistence | {enabled,loaded}' "$MBODY")"
  else
    fail persist-reload "$(jq -c .persistence "$MBODY" | cut -c1-300)"
  fi
fi
if need_mgmt counters counters-survive; then
  row="$(jq -c --arg m "$MODEL_A" '[.models[]? | select(.model == $m)][0] // {} | {rpd,tpd}' "$MBODY")"
  if jq -e '.rpd.used >= 1' <<<"$row" >/dev/null 2>&1; then
    pass counters-survive "after restart $MODEL_A $(jq -c '{rpd:.rpd.used,tpd:.tpd.used}' <<<"$row")"
  else
    fail counters-survive "after restart $MODEL_A row=$row"
  fi
fi
# (6) debug-force-error: the plugin misbehaves; the host must stay up and answer cleanly
M=$(logmark)
chat "$ALIAS"
alive=no; kill -0 "$PID" 2>/dev/null && alive=yes
info debug-alias "client saw http $CODE: $(head -c 300 "$BODY" | redact | tr '\n' ' ')"
if [ "$alive" = yes ] && [ "$CODE" -ge 400 ] 2>/dev/null; then
  pass debug-fallthrough "plugin error -> clean failure for the alias (http $CODE), host alive"
else
  fail debug-fallthrough "http $CODE host alive: $alive"
fi
excerpt "$M" 'panic|plugin|tierbridge|executor' 6
chat "$MODEL_B"
if [ "$CODE" = 200 ]; then pass debug-non-alias "$MODEL_B -> 200 with the plugin erroring"; else fail debug-non-alias "http $CODE $(errsum)"; fi
stop_cpa

echo "SUMMARY requests=$REQS/$MAX_REQS fails=$FAILS"
[ "$FAILS" -eq 0 ]
