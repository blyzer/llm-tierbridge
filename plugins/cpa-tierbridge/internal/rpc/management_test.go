package rpc

import (
	"encoding/json"
	"fmt"
	"net/http"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

// hostManagementRequest serialises a management.handle request like the host: the embedded
// pluginapi.ManagementRequest (capitalised keys, Body base64) plus host_callback_id.
func hostManagementRequest(t *testing.T, method, path string, query map[string][]string) []byte {
	t.Helper()
	raw, err := wire.Marshal(map[string]any{
		"Method": method, "Path": path, "Query": query,
		"Headers":          map[string][]string{"Authorization": {"Bearer SECRET-TOKEN"}, "X-Api-Key": {"SECRET-KEY"}},
		"Body":             []byte(`{"prompt":"SECRET-PROMPT"}`),
		"host_callback_id": "cb-9",
	})
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

// manage calls management.handle and returns the decoded response plus the raw envelope.
func manage(t *testing.T, h *Handler, path string, query map[string][]string) (wire.ManagementResponse, []byte) {
	t.Helper()
	raw := h.Handle(wire.MethodManagementHandle, hostManagementRequest(t, "GET", path, query))
	var resp wire.ManagementResponse
	if env := decode(t, raw, &resp); !env.OK {
		t.Fatalf("management.handle error envelope: %+v", env.Error)
	}
	return resp, raw
}

func getJSON(t *testing.T, h *Handler, path string, query map[string][]string) map[string]any {
	t.Helper()
	resp, _ := manage(t, h, path, query)
	if resp.StatusCode != 200 || resp.Headers["Content-Type"][0] != "application/json" {
		t.Fatalf("%s = %d %v %s", path, resp.StatusCode, resp.Headers, resp.Body)
	}
	for _, secret := range []string{"SECRET-TOKEN", "SECRET-KEY", "SECRET-PROMPT", "Authorization"} {
		if strings.Contains(string(resp.Body), secret) {
			t.Fatalf("%s leaked request data %q: %s", path, secret, resp.Body)
		}
	}
	var out map[string]any
	if err := json.Unmarshal(resp.Body, &out); err != nil {
		t.Fatalf("%s body is not JSON: %v\n%s", path, err, resp.Body)
	}
	return out
}

func keysOf(m map[string]any) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	slices.Sort(keys)
	return keys
}

func wantKeys(t *testing.T, what string, m map[string]any, want ...string) {
	t.Helper()
	slices.Sort(want)
	if got := keysOf(m); !slices.Equal(got, want) {
		t.Errorf("%s keys = %v, want %v", what, got, want)
	}
}

func obj(t *testing.T, v any) map[string]any {
	t.Helper()
	m, ok := v.(map[string]any)
	if !ok {
		t.Fatalf("%v is not an object", v)
	}
	return m
}

func arr(t *testing.T, v any) []any {
	t.Helper()
	a, ok := v.([]any)
	if !ok {
		t.Fatalf("%v is not an array", v)
	}
	return a
}

const richRanking = `{"provider":"gw","id":"a","model":"tb-a","contextWindow":1000,"maxTokens":100,"supportsTools":true,"input":["text"],
  "cost":{"status":"free","input":0,"output":0},"quota":{"rpm":5,"rpd":50,"tpm":1000,"tpd":9000}},
 {"provider":"gw","id":"b","model":"tb-b","cost":{"status":"known","input":1,"output":2}},
 {"provider":"gw","id":"c","model":"tb-c","cost":{"status":"unknown"}}`

func richPlugin(t *testing.T) *Handler {
	t.Helper()
	state := filepath.Join(t.TempDir(), "state.json")
	h, _ := newPlugin(t, wrap(richRanking), fmt.Sprintf("daily-budget-usd: 5\ncooldown-seconds: 60\nstate-path: %q\n", state))
	route(t, h, aliasModel, chatBody(40, ""), nil)
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-a", "Provider": "gw", "Detail": detail(10, 20, 30)}))
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-b", "Provider": "gw", "Detail": detail(1_000_000, 0, 1_000_000),
		"Failed": true, "Failure": map[string]any{"StatusCode": 429}}))
	return h
}

func TestManagementRegister(t *testing.T) {
	h := NewHandler()
	raw := h.Handle(wire.MethodManagementRegister, nil)
	var reg wire.ManagementRegistrationResponse
	decode(t, raw, &reg)
	var got []string
	for _, r := range reg.Routes {
		got = append(got, r.Method+" "+r.Path)
		if r.Menu != "" {
			t.Errorf("route %s has a Menu %q", r.Path, r.Menu)
		}
		if r.Description == "" {
			t.Errorf("route %s has no description", r.Path)
		}
	}
	want := []string{"GET /tierbridge/status", "GET /tierbridge/candidates", "GET /tierbridge/counters", "GET /tierbridge/budget", "GET /tierbridge/decisions"}
	if !slices.Equal(got, want) || len(reg.Resources) != 0 {
		t.Errorf("routes = %v resources %v", got, reg.Resources)
	}
}

func TestManagementResponseIsBase64OnTheWire(t *testing.T) {
	h := richPlugin(t)
	_, raw := manage(t, h, "/tierbridge/budget", nil)
	var env struct {
		Result map[string]json.RawMessage
	}
	if err := json.Unmarshal(raw, &env); err != nil {
		t.Fatal(err)
	}
	wantKeys(t, "ManagementResponse", map[string]any{"StatusCode": 1, "Headers": 1, "Body": 1}, keysOf(map[string]any{"StatusCode": 1, "Headers": 1, "Body": 1})...)
	var body string
	if err := json.Unmarshal(env.Result["Body"], &body); err != nil || strings.HasPrefix(body, "{") {
		t.Errorf("Body is not a base64 string: %s (%v)", env.Result["Body"], err)
	}
	if len(env.Result) != 3 {
		t.Errorf("ManagementResponse keys = %v", env.Result)
	}
}

func TestManagementStatusShape(t *testing.T) {
	h := richPlugin(t)
	st := getJSON(t, h, "/tierbridge/status", nil)
	wantKeys(t, "status", st, "plugin", "router", "config", "configError", "lastError", "lastErrorAt", "lastUsageModels", "persistence")
	wantKeys(t, "plugin", obj(t, st["plugin"]), "name", "version", "abiVersion", "schemaVersion", "hostCommit")
	plugin := obj(t, st["plugin"])
	if plugin["version"] != PluginVersion || plugin["abiVersion"] != float64(1) || plugin["schemaVersion"] != float64(10) {
		t.Errorf("plugin = %v", plugin)
	}
	router := obj(t, st["router"])
	wantKeys(t, "router", router, "path", "revision", "aliases", "lastLoadError")
	rev := obj(t, router["revision"])
	wantKeys(t, "revision", rev, "catalogSha256", "quotaSourceSha256", "fileSha256", "modTime", "profile", "loadedAt")
	if rev["catalogSha256"] != "abc123" || rev["profile"] != "free-first" || rev["fileSha256"] == "" {
		t.Errorf("revision = %v", rev)
	}
	wantKeys(t, "config", obj(t, st["config"]), "routerFile", "alias", "dailyBudgetUsd", "cooldownSeconds", "statePath", "decisionsKeep", "debugForceError")
	if obj(t, st["config"])["dailyBudgetUsd"] != float64(5) {
		t.Errorf("config = %v", st["config"])
	}
	usage := arr(t, st["lastUsageModels"])
	if len(usage) != 2 {
		t.Fatalf("lastUsageModels = %v", usage)
	}
	wantKeys(t, "lastUsageModels[0]", obj(t, usage[0]), "time", "model", "alias", "provider", "failed", "status", "tokens", "matched")
	wantKeys(t, "persistence", obj(t, st["persistence"]), "enabled", "path", "loaded", "loadedAt", "dirty", "saves", "lastSaveAt")
	wantKeys(t, "router.aliases[0]", obj(t, arr(t, router["aliases"])[0]), "alias", "profile", "candidates", "unpriced")
}

func TestManagementStatusReportsAnUnreadableRouterFile(t *testing.T) {
	h, _ := newPlugin(t, "", "")
	st := getJSON(t, h, "/tierbridge/status", nil)
	if obj(t, st["router"])["lastLoadError"] == "" {
		// The store only reports a problem once a route asked for the file.
		route(t, h, aliasModel, chatBody(10, ""), nil)
		st = getJSON(t, h, "/tierbridge/status", nil)
	}
	if obj(t, st["router"])["lastLoadError"] == "" {
		t.Errorf("router = %v, want the last load error", st["router"])
	}
}

func TestManagementCandidatesShape(t *testing.T) {
	h := richPlugin(t)
	doc := getJSON(t, h, "/tierbridge/candidates", nil)
	wantKeys(t, "candidates", doc, "alias", "profile", "candidates")
	cands := arr(t, doc["candidates"])
	if len(cands) != 3 {
		t.Fatalf("candidates = %v", cands)
	}
	first := obj(t, cands[0])
	wantKeys(t, "candidate", first, "rank", "provider", "model", "id", "contextWindow", "maxTokens", "supportsTools", "input", "cost", "unpriced", "limits")
	wantKeys(t, "cost", obj(t, first["cost"]), "status", "input", "output")
	wantKeys(t, "limits", obj(t, first["limits"]), "rpm", "rpd", "tpm", "tpd")
	lim := obj(t, first["limits"])
	if lim["rpm"] != float64(5) || lim["rpd"] != float64(50) || lim["tpm"] != float64(1000) || lim["tpd"] != float64(9000) {
		t.Errorf("limits = %v", lim)
	}
	if lim := obj(t, obj(t, cands[1])["limits"]); lim["rpm"] != nil || lim["tpd"] != nil {
		t.Errorf("unlimited must read null: %v", lim)
	}
	if first["model"] != "tb-a" || obj(t, cands[2])["model"] != "tb-c" || obj(t, cands[2])["unpriced"] != true || first["rank"] != float64(1) {
		t.Errorf("order/unpriced wrong: %v", cands)
	}
}

func TestManagementCountersShape(t *testing.T) {
	h := richPlugin(t)
	doc := getJSON(t, h, "/tierbridge/counters", nil)
	wantKeys(t, "counters", doc, "day", "now", "models")
	models := arr(t, doc["models"])
	a := obj(t, models[0])
	wantKeys(t, "model", a, "provider", "model", "rpm", "rpd", "tpm", "tpd", "cooldownRemainingSeconds")
	wantKeys(t, "rpm", obj(t, a["rpm"]), "used", "limit")
	if obj(t, a["rpm"])["used"] != float64(1) || obj(t, a["rpm"])["limit"] != float64(5) ||
		obj(t, a["tpm"])["used"] != float64(30) || obj(t, a["tpd"])["limit"] != float64(9000) {
		t.Errorf("tb-a counters = %v", a)
	}
	b := obj(t, models[1])
	if b["cooldownRemainingSeconds"] != float64(60) || obj(t, b["rpm"])["limit"] != nil {
		t.Errorf("tb-b counters = %v", b)
	}
}

func TestManagementBudgetShape(t *testing.T) {
	h := richPlugin(t)
	doc := getJSON(t, h, "/tierbridge/budget", nil)
	wantKeys(t, "budget", doc, "day", "dailyBudgetUsd", "spentUsd", "remainingUsd", "unpriced")
	if doc["dailyBudgetUsd"] != float64(5) || doc["spentUsd"] != float64(1) || doc["remainingUsd"] != float64(4) || doc["day"] != "2026-11-27" {
		t.Errorf("budget = %v", doc)
	}
	if u := arr(t, doc["unpriced"]); len(u) != 1 || u[0] != "tb-c" {
		t.Errorf("unpriced = %v", u)
	}
}

func TestManagementDecisionsShapeAndLimit(t *testing.T) {
	h := richPlugin(t)
	for range 6 { // the first route of richPlugin plus these six: the seventh exceeds tb-a's rpm of 5
		route(t, h, aliasModel, chatBody(40, ""), nil)
	}
	doc := getJSON(t, h, "/tierbridge/decisions", nil)
	wantKeys(t, "decisions", doc, "limit", "decisions")
	all := arr(t, doc["decisions"])
	if len(all) != 7 {
		t.Fatalf("decisions = %d", len(all))
	}
	d := obj(t, all[0])
	wantKeys(t, "decision", d, "time", "requestedModel", "chosen", "reason", "skipped", "estimatedTokens")
	wantKeys(t, "chosen", obj(t, d["chosen"]), "provider", "model", "rank")
	if d["requestedModel"] != aliasModel || d["estimatedTokens"] != float64(10) {
		t.Errorf("decision = %v", d)
	}
	// Newest first: the last request found tb-a at its rpm limit... tb-b is cooling down, so tb-c serves.
	if obj(t, d["chosen"])["model"] != "tb-c" {
		t.Errorf("newest decision = %v", d)
	}
	skipped := arr(t, d["skipped"])
	wantKeys(t, "skipped[0]", obj(t, skipped[0]), "rank", "provider", "model", "reason")
	two := getJSON(t, h, "/tierbridge/decisions", map[string][]string{"limit": {"2"}})
	if n := len(arr(t, two["decisions"])); n != 2 || two["limit"] != float64(2) {
		t.Errorf("limit=2 returned %d decisions: %v", n, two)
	}
	for _, bad := range []string{"0", "-1", "abc", ""} {
		resp, _ := manage(t, h, "/tierbridge/decisions", map[string][]string{"limit": {bad}})
		if resp.StatusCode != 400 || !strings.Contains(string(resp.Body), `"error"`) {
			t.Errorf("limit=%q = %d %s", bad, resp.StatusCode, resp.Body)
		}
	}
}

func TestManagementDecisionWithoutAPick(t *testing.T) {
	h, _ := newPlugin(t, wrap(`{"provider":"gw","model":"tiny","contextWindow":5}`), "")
	route(t, h, aliasModel, chatBody(400, ""), nil)
	d := obj(t, arr(t, getJSON(t, h, "/tierbridge/decisions", nil)["decisions"])[0])
	if d["chosen"] != nil || len(arr(t, d["skipped"])) != 1 {
		t.Errorf("decision = %v, want chosen null and one skip", d)
	}
}

func TestManagementUnknownPathIs404JSON(t *testing.T) {
	h := NewHandler()
	for _, path := range []string{"/tierbridge/nope", "/", "", "/tierbridge", "/tierbridge/status/extra", "/tierbridge/statusx"} {
		resp, _ := manage(t, h, path, nil)
		if resp.StatusCode != 404 || resp.Headers["Content-Type"][0] != "application/json" || string(resp.Body) != `{"error":"not found"}` {
			t.Errorf("%q = %d %v %s", path, resp.StatusCode, resp.Headers, resp.Body)
		}
	}
	// Garbage and empty requests answer 404 JSON too.
	for _, raw := range []string{"", "{{", "null", "[]"} {
		var resp wire.ManagementResponse
		if env := decode(t, h.Handle(wire.MethodManagementHandle, []byte(raw)), &resp); !env.OK || resp.StatusCode != 404 {
			t.Errorf("%q = %+v %+v", raw, env, resp)
		}
	}
}

func TestManagementPathsAndMethods(t *testing.T) {
	h := NewHandler()
	// The host may hand over the path with its management prefix in front.
	for _, path := range []string{"/v0/management/tierbridge/status", "/tierbridge/status/", "/tierbridge/status?x=1"} {
		if resp, _ := manage(t, h, path, nil); resp.StatusCode != 200 {
			t.Errorf("%q = %d %s", path, resp.StatusCode, resp.Body)
		}
	}
	var resp wire.ManagementResponse
	decode(t, h.Handle(wire.MethodManagementHandle, hostManagementRequest(t, "POST", "/tierbridge/status", nil)), &resp)
	if resp.StatusCode != http.StatusMethodNotAllowed || !strings.Contains(string(resp.Body), `"error"`) {
		t.Errorf("POST = %d %s", resp.StatusCode, resp.Body)
	}
}

func TestManagementRoutesRecoverFromPanics(t *testing.T) {
	h := richPlugin(t)
	h.beforeManagement = func(string) { panic("boom") }
	resp, _ := manage(t, h, "/tierbridge/status", nil)
	if resp.StatusCode != 500 || resp.Headers["Content-Type"][0] != "application/json" || string(resp.Body) != `{"error":"internal error"}` {
		t.Errorf("panic response = %d %v %s", resp.StatusCode, resp.Headers, resp.Body)
	}
	if !strings.Contains(h.Status().LastError, "management.handle: panic: boom") {
		t.Errorf("lastError = %q", h.Status().LastError)
	}
	h.beforeManagement = nil
	if resp, _ := manage(t, h, "/tierbridge/status", nil); resp.StatusCode != 200 {
		t.Errorf("the handler did not survive: %d", resp.StatusCode)
	}
	// A nil accountant panics inside a view: still a JSON 500, never an error envelope.
	h.acct = nil
	if resp, _ := manage(t, h, "/tierbridge/counters", nil); resp.StatusCode != 500 {
		t.Errorf("counters with a broken accountant = %d %s", resp.StatusCode, resp.Body)
	}
}

// TestConcurrentRouteUsageAndManagement is meant to run under -race: it hammers every entry point at once.
func TestConcurrentRouteUsageAndManagement(t *testing.T) {
	state := filepath.Join(t.TempDir(), "state.json")
	h, router := newPlugin(t, wrap(richRanking), fmt.Sprintf("state-path: %q\ndaily-budget-usd: 100\n", state))
	h.now = time.Now // real time: windows and timers move while the test runs
	body := hostRouteRequest(t, aliasModel, chatBody(400, ""), nil)
	okUsage := hostUsageRecord(t, map[string]any{"Model": "tb-a", "Provider": "gw", "Detail": detail(10, 10, 20)})
	failUsage := hostUsageRecord(t, map[string]any{"Model": "tb-b", "Failed": true, "Failure": map[string]any{"StatusCode": 429},
		"ResponseHeaders": http.Header{"Retry-After": {"1"}}})
	paths := []string{"/tierbridge/status", "/tierbridge/candidates", "/tierbridge/counters", "/tierbridge/budget", "/tierbridge/decisions"}

	var wg sync.WaitGroup
	run := func(n int, f func(i int)) {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := range n {
				f(i)
			}
		}()
	}
	for range 4 {
		run(150, func(int) { h.Handle(wire.MethodModelRoute, body) })
		run(150, func(i int) {
			h.Handle(wire.MethodUsageHandle, okUsage)
			if i%10 == 0 {
				h.Handle(wire.MethodUsageHandle, failUsage)
				h.Handle(wire.MethodUsageHandle, []byte("garbage"))
			}
		})
		run(100, func(i int) {
			h.Handle(wire.MethodManagementHandle, hostManagementRequest(t, "GET", paths[i%len(paths)], nil))
		})
	}
	run(20, func(i int) {
		h.Handle(wire.MethodPluginReconfigure, lifecycle(t, fmt.Sprintf("router-file: %q\nstate-path: %q\ndaily-budget-usd: %d\n", router, state, 50+i)))
	})
	wg.Wait()
	h.Handle(wire.MethodPluginShutdown, nil)

	st := getJSON(t, h, "/tierbridge/status", nil)
	if p := obj(t, st["persistence"]); p["saves"].(float64) < 1 {
		t.Errorf("nothing was persisted: %v", p)
	}
	if n := len(h.Status().LastUsageModels); n != MaxUsageSeen {
		t.Errorf("lastUsageModels = %d", n)
	}
}
