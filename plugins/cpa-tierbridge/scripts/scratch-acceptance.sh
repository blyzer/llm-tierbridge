#!/usr/bin/env bash
# Scratch-CPA acceptance for the cpa-tierbridge plugin (slice 6).
#
# Starts a THROWAWAY CLIProxyAPI on 127.0.0.1:$PORT (cwd, config, auth dir, plugins dir, router file
# and state file all inside one mktemp dir), loads ONLY our dylib, and sends at most MAX_REQS (10) tiny
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
MAX_REQS=10
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
tb export cpa-router --catalog "$CATALOG" --gateway "$GATEWAY" --name "$NAME" --alias-prefix "$ALIAS_PREFIX" \
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

# ================= phase 1: plugin as configured =================
write_config false
start_cpa

# (7a) load / register / capabilities
if grep -q 'pluginhost: plugin registered' "$TMP/cpa.out"; then
  pass plugin-registered "$(grep -m1 'pluginhost: plugin registered' "$TMP/cpa.out" | redact | grep -oE 'plugin_(name|id|version)=[^ ]+' | paste -sd' ' - | cut -c1-120)"
else
  fail plugin-registered "no 'pluginhost: plugin registered' line"
fi
[ "$VERBOSE" != 1 ] || awk '/pluginhost/ { print "      | " substr($0, 1, 300) }' "$TMP/cpa.out" | redact
mgmt /v0/management/plugins auth
if [ "$MCODE" = 200 ]; then
  info plugin-caps "$(jq -c '[.. | objects | select(.id? == "tierbridge" or .name? == "tierbridge")] | max_by(length) | {registered, enabled, effective_enabled, supports_oauth, supports_quota, keys: keys, caps: with_entries(select(.key | test("capab|router|usage|management|executor|schedul|hook")))}' "$MBODY" 2>/dev/null | cut -c1-600 | redact)"
else
  info plugin-caps "GET /v0/management/plugins -> $MCODE"
fi

# (1) alias -> top fitting candidate
M=$(logmark)
chat "$ALIAS"
if [ "$CODE" = 200 ] && [ "$RMODEL" = "$ID_A" ]; then pass route-top "alias -> $ID_A (200)"; else fail route-top "http $CODE model=${RMODEL:-?} want $ID_A $(errsum)"; fi
excerpt "$M" 'tb-|route|router|model' 6
# (2) rpm-1 candidate exhausted: next request skips it
chat "$ALIAS"
if [ "$CODE" = 200 ] && [ "$RMODEL" = "$ID_B" ]; then pass rpm-skip "2nd alias request skipped $ID_A -> $ID_B"; else fail rpm-skip "http $CODE model=${RMODEL:-?} want $ID_B $(errsum)"; fi
# (3) non-alias model is unaffected (even though A is out of quota for the alias)
chat "$MODEL_A"
if [ "$CODE" = 200 ] && [ "$RMODEL" = "$ID_A" ]; then pass non-alias "$MODEL_A -> 200 $ID_A (router not involved)"; else fail non-alias "http $CODE model=${RMODEL:-?} want $ID_A $(errsum)"; fi

# (5) management endpoints (answer with the secret, 401 without)
mgmt /v0/management/config auth; base_with="$MCODE"
mgmt /v0/management/config noauth; base_without="$MCODE"
if [ "$base_with" = 200 ] && [ "$base_without" = 401 ]; then pass mgmt-baseline "host /config: 200 with secret, 401 without"; else fail mgmt-baseline "host /config: with=$base_with without=$base_without"; fi
for ep in status candidates counters budget decisions; do
  mgmt "/v0/management/tierbridge/$ep" auth; w="$MCODE"
  mgmt "/v0/management/tierbridge/$ep" noauth; n="$MCODE"
  if [ "$w" = 200 ] && [ "$n" = 401 ]; then
    pass "mgmt-$ep" "200 with secret, 401 without"
  elif [ "$w" = 404 ]; then
    if [ "$REQUIRE_MGMT" = 1 ]; then fail "mgmt-$ep" "404 (route not in this build); without secret: $n"; else warn "mgmt-$ep" "404 (route not in this build); without secret: $n"; fi
  else
    fail "mgmt-$ep" "with=$w without=$n"
  fi
done
dump_mgmt() { # label: VERBOSE only; compact redacted bodies of the tierbridge management routes
  [ "$VERBOSE" = 1 ] || return 0
  local ep
  for ep in status candidates counters budget decisions; do
    mgmt "/v0/management/tierbridge/$ep" auth
    echo "      | [$1] $ep -> $MCODE $(jq -c . "$MBODY" 2>/dev/null | redact | cut -c1-1800)"
  done
}
dump_mgmt after-requests

need_mgmt() { # endpoint check-name: fetch it into MBODY; on non-200 FAIL (REQUIRE_MGMT=1) or WARN and return 1
  mgmt "/v0/management/tierbridge/$1" auth
  [ "$MCODE" = 200 ] && return 0
  if [ "$REQUIRE_MGMT" = 1 ]; then fail "$2" "GET $1 -> $MCODE"; else warn "$2" "GET $1 -> $MCODE (route not in this build)"; fi
  return 1
}
PROVIDER="$(jq -r '.provider' <<<"$CAND_A")"
FAIL_MODEL="${ALIAS_PREFIX}${FAIL_UPSTREAM_ID:-}"

# (b) the usage records exactly as the host sends them, and whether the plugin matched them
if need_mgmt status usage-match; then
  info usage-records "lastUsageModels (newest first): $(jq -c '[.lastUsageModels[]? | {model,alias,provider,failed,status,tokens,matched}]' "$MBODY" | redact | cut -c1-900)"
  if jq -e --arg a "$ALIAS" --arg id "$ID_A" --arg p "$PROVIDER" \
    '[.lastUsageModels[]? | select(.alias == $a and .model == $id and .provider == $p and .matched == true)] | length > 0' "$MBODY" >/dev/null; then
    pass usage-match "routed request: Model=$ID_A Alias=$ALIAS Provider=$PROVIDER matched=true (host sends the upstream id, not the tb- name)"
  else
    fail usage-match "no matched record with alias=$ALIAS model=$ID_A provider=$PROVIDER"
  fi
fi
# (c) counters of the chosen model (A: alias request + the non-alias request)
if need_mgmt counters counters-used; then
  row="$(jq -c --arg m "$MODEL_A" '[.models[]? | select(.model == $m)][0] // {} | {rpm,rpd,tpm,tpd}' "$MBODY")"
  if jq -e '.rpm.used >= 1 and .rpm.limit == 1 and .rpd.used >= 1 and .tpm.used > 0 and .tpd.used > 0' <<<"$row" >/dev/null 2>&1; then
    pass counters-used "$MODEL_A $(jq -c '{rpm:.rpm.used,rpmLimit:.rpm.limit,rpd:.rpd.used,tpm:.tpm.used,tpd:.tpd.used}' <<<"$row")"
  else
    fail counters-used "$MODEL_A row=$row"
  fi
fi
# (e) budget
if need_mgmt budget budget; then
  if jq -e '.dailyBudgetUsd == 1 and (.spentUsd | type) == "number" and .remainingUsd <= 1' "$MBODY" >/dev/null 2>&1; then
    pass budget "$(jq -c '{day,dailyBudgetUsd,spentUsd,remainingUsd,unpriced}' "$MBODY" | cut -c1-200)"
  else
    fail budget "$(jq -c . "$MBODY" | cut -c1-200)"
  fi
fi
# decisions: request 2 skipped A for rpm
if need_mgmt decisions decisions-rpm; then
  if jq -e --arg m "$MODEL_B" --arg a "$MODEL_A" \
    '[.decisions[]? | select(.chosen.model == $m and ([.skipped[]? | select(.model == $a and .reason == "rpm")] | length > 0))] | length > 0' "$MBODY" >/dev/null 2>&1; then
    pass decisions-rpm "chose $MODEL_B, skipped $MODEL_A (rpm)"
  else
    fail decisions-rpm "$(jq -c '[.decisions[]? | {chosen: .chosen.model, reason}]' "$MBODY" | cut -c1-300)"
  fi
fi
usage_queue() {
  mgmt "/v0/management/usage-queue?count=20" auth   # host usage records (what the plugin's usage hook also sees)
  if [ "$MCODE" = 200 ]; then
    info usage-queue "host usage-queue models: $(jq -c '[.. | objects | (.model? // .alias? // empty) | strings] | unique | .[0:8]' "$MBODY" 2>/dev/null | redact | cut -c1-240)"
  else
    info usage-queue "GET usage-queue -> $MCODE"
  fi
}
usage_queue

# (6a) routed model fails: top candidate is a model the provider does not have
BAD1="${ALIAS_PREFIX}does-not-exist"
write_router "$TMP/router.json" "$(jq -c --arg m "$BAD1" '[(.[0] | .model=$m | .id="does-not-exist" | .quota=null)] + .' <<<"$(sel_json)")"
M=$(logmark)
chat "$ALIAS"
info fail-routed "A: top=$BAD1 (no quota) -> http $CODE model=${RMODEL:-none} $(errsum)"
excerpt "$M" 'does-not-exist|model_not_found|route|retry|auth_not_found' 12
# (6b) same, but the bad candidate has rpm 1: a router RE-ASK would skip it and reach the next candidate (A)
BAD2="${ALIAS_PREFIX}does-not-exist-2"
write_router "$TMP/router.json" "$(jq -c --arg m "$BAD2" '[(.[0] | .model=$m | .id="does-not-exist-2" | .quota={rpm:1,rpd:null})] + .' <<<"$(sel_json)")"
M=$(logmark)
chat "$ALIAS"
info fail-reask "B: top=$BAD2 (rpm 1) -> http $CODE model=${RMODEL:-none} $(errsum)"
excerpt "$M" 'does-not-exist|model_not_found|route|retry|auth_not_found' 12
if [ "$CODE" = 200 ] && [ "$RMODEL" = "$ID_A" ]; then
  info fail-reask "client got 200 from $ID_A: the router was asked again (or the host fell back) after the failure"
else
  info fail-reask "client did not get a fallback answer: the router is not re-asked for the same request"
fi

# (d) cooldown: the routed model EXISTS but the upstream fails it (FAIL_UPSTREAM_ID: an id answering 429/5xx today).
# Request 1 hits it and fails; the NEXT alias request must skip it with reason "cooldown".
if [ -n "${FAIL_UPSTREAM_ID:-}" ]; then
  write_router "$TMP/router.json" "$(jq -c --arg id "$FAIL_UPSTREAM_ID" --arg m "$FAIL_MODEL" '[(.[0] | .model=$m | .id=$id | .quota=null)] + .' <<<"$(sel_json)")"
  M=$(logmark)
  chat "$ALIAS"
  info fail-upstream "top=$FAIL_MODEL (upstream errors) -> http $CODE model=${RMODEL:-none} $(errsum); the router is not re-asked, the error reaches the client"
  excerpt "$M" "$FAIL_UPSTREAM_ID|retry|route|status" 14
  if need_mgmt status usage-failed; then
    info usage-records "failed record: $(jq -c '[.lastUsageModels[]? | select(.failed)][0] // "none" | if type == "object" then {model,alias,provider,failed,status,matched} else . end' "$MBODY" | redact | cut -c1-300)"
  fi
  for _ in $(seq 1 12); do   # the usage record reaches the plugin asynchronously
    mgmt "/v0/management/tierbridge/counters" auth
    jq -e --arg m "$FAIL_MODEL" '[.models[]? | select(.model == $m and .cooldownRemainingSeconds > 0)] | length > 0' "$MBODY" >/dev/null 2>&1 && break
    sleep 0.5
  done
  chat "$ALIAS"
  if [ "$CODE" = 200 ] && [ "$RMODEL" = "$ID_A" ]; then
    mgmt "/v0/management/tierbridge/decisions" auth
    if jq -e --arg f "$FAIL_MODEL" --arg a "$MODEL_A" \
      '.decisions[0] | .chosen.model == $a and ([.skipped[]? | select(.model == $f and .reason == "cooldown")] | length > 0)' "$MBODY" >/dev/null 2>&1; then
      pass cooldown "next alias request skipped $FAIL_MODEL (reason cooldown) -> $ID_A; $(jq -c '.decisions[0] | {reason}' "$MBODY" | cut -c1-200)"
    else
      fail cooldown "200 from $ID_A but decisions[0]=$(jq -c '.decisions[0] | {chosen: .chosen.model, reason, skipped}' "$MBODY" 2>/dev/null | cut -c1-300)"
    fi
  else
    fail cooldown "2nd request: http $CODE model=${RMODEL:-none} $(errsum) (want 200 from $ID_A)"
  fi
  mgmt "/v0/management/tierbridge/counters" auth
  info cooldown "counters: $(jq -c --arg m "$FAIL_MODEL" '[.models[]? | select(.model == $m)][0] | {model, rpd, cooldownRemainingSeconds}' "$MBODY" 2>/dev/null | cut -c1-240)"
else
  warn cooldown "skipped: set FAIL_UPSTREAM_ID to a served id the upstream fails today"
fi
sleep 1.5   # let the usage record of the last request reach the plugin; the periodic flush is 5 s away
mgmt "/v0/management/tierbridge/counters" auth
LIVE_TPD_A="$(jq -r --arg m "$MODEL_A" '[.models[]? | select(.model == $m)][0].tpd.used // 0' "$MBODY" 2>/dev/null || echo 0)"
mgmt "/v0/management/tierbridge/status" auth
LIVE_PERSIST="$(jq -c '.persistence | {dirty, saves, lastSaveAt}' "$MBODY" 2>/dev/null || echo '{}')"
stop_cpa

# (f) persistence. Right after SIGTERM: is the state file there, 0600, and does it hold what happened in this run?
# (The host never sends the plugin.shutdown RPC, it calls the C shutdown hook; the plugin otherwise only writes
# SaveInterval = 5 s after a change, so a stop right after the requests can lose counters and cooldowns.)
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
  [ "$file_tpd" = "$LIVE_TPD_A" ] || missing="$missing tpd($MODEL_A: file $file_tpd, live $LIVE_TPD_A)"
  info persist-flush "just before SIGTERM (live status.persistence) $LIVE_PERSIST; file savedAt=$(jq -r '.savedAt' "$ST" 2>/dev/null)"
  jq -e --arg m "$MODEL_B" '[.models | to_entries[] | select(.key | endswith($m))] | length > 0' "$ST" >/dev/null 2>&1 || missing="$missing model($MODEL_B)"
  if [ -n "${FAIL_UPSTREAM_ID:-}" ]; then
    jq -e --arg m "$FAIL_MODEL" '[.models | to_entries[] | select(.key | endswith($m)) | .value | select(.cooldownUntil != null and .cooldownUntil != "0001-01-01T00:00:00Z")] | length > 0' "$ST" >/dev/null 2>&1 || missing="$missing cooldown($FAIL_MODEL)"
  fi
  if [ -z "$missing" ]; then
    pass persist-flush "state at SIGTERM holds the whole run (tpd, 2nd model, cooldown)"
  elif [ "$REQUIRE_MGMT" = 1 ]; then
    fail persist-flush "state at SIGTERM lacks:$missing (changes <5 s old are not flushed when the host stops)"
  else
    warn persist-flush "state at SIGTERM lacks:$missing (changes <5 s old are not flushed when the host stops)"
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
M=$(logmark)
chat "$ALIAS"
if [ "$CODE" = 400 ] && jq -e '(.error.code // "") == "model_not_found"' "$BODY" >/dev/null 2>&1; then
  pass debug-fallthrough "plugin panic -> host fell through: 400 model_not_found (host alive: $(kill -0 "$PID" 2>/dev/null && echo yes || echo NO))"
else
  fail debug-fallthrough "http $CODE $(errsum) (want 400 model_not_found)"
fi
excerpt "$M" 'panic|plugin|tierbridge' 6
chat "$MODEL_B"
if [ "$CODE" = 200 ] && [ "$RMODEL" = "$ID_B" ]; then pass debug-non-alias "$MODEL_B -> 200 with the plugin erroring"; else fail debug-non-alias "http $CODE model=${RMODEL:-?} $(errsum)"; fi
# shutdown flush: the change of the request above was written at once (first write after a start); the change
# of the next one is < 5 s old, so it is still pending when SIGTERM arrives. The file must end up equal to the
# live counters (live status.persistence.dirty should read true here).
chat "$MODEL_A"
sleep 1.2
mgmt "/v0/management/tierbridge/counters" auth
LIVE_TPD_A2="$(jq -r --arg m "$MODEL_A" '[.models[]? | select(.model == $m)][0].tpd.used // 0' "$MBODY" 2>/dev/null || echo 0)"
mgmt "/v0/management/tierbridge/status" auth
LIVE_PERSIST2="$(jq -c '.persistence | {dirty, saves}' "$MBODY" 2>/dev/null || echo '{}')"
stop_cpa
file_tpd2="$(jq -r --arg m "$MODEL_A" '[.models | to_entries[] | select(.key | endswith($m)) | .value.tpd] | first // 0' "$ST" 2>/dev/null || echo 0)"
if [ "$file_tpd2" = "$LIVE_TPD_A2" ]; then
  if jq -e '.dirty == true' <<<"$LIVE_PERSIST2" >/dev/null 2>&1; then
    pass persist-shutdown "pending change flushed at stop: file tpd($MODEL_A)=$file_tpd2 equals live (live persistence $LIVE_PERSIST2)"
  else
    warn persist-shutdown "inconclusive: nothing was pending at SIGTERM (live persistence $LIVE_PERSIST2), file tpd=$file_tpd2"
  fi
elif [ "$REQUIRE_MGMT" = 1 ]; then
  fail persist-shutdown "changes < 5 s old are lost when the host stops: file tpd($MODEL_A)=$file_tpd2, live=$LIVE_TPD_A2 (live persistence $LIVE_PERSIST2)"
else
  warn persist-shutdown "changes < 5 s old are lost when the host stops: file tpd($MODEL_A)=$file_tpd2, live=$LIVE_TPD_A2 (live persistence $LIVE_PERSIST2)"
fi

echo "SUMMARY requests=$REQS/$MAX_REQS fails=$FAILS"
[ "$FAILS" -eq 0 ]
