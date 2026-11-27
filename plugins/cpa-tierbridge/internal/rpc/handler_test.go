package rpc

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

var clock = time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)

const rankingJSON = `{"schema":"tierbridge-router/1","catalogSha256":"abc123","aliases":[{"alias":"tierbridge/auto","profile":"free-first","candidates":[
 {"provider":"small","model":"s","contextWindow":100,"supportsTools":true,"input":["text","image"]},
 {"provider":"notools","model":"n","contextWindow":100000,"supportsTools":false,"input":["text","image"]},
 {"provider":"textonly","model":"t","contextWindow":100000,"supportsTools":true,"input":["text"]},
 {"provider":"big","model":"b","contextWindow":1000000,"supportsTools":true,"input":["text","image"]}
]}]}`

const aliasModel = "tierbridge/auto"

// wrap puts candidate JSON into a valid router file holding the default alias.
func wrap(candidates string) string {
	return `{"schema":"tierbridge-router/1","catalogSha256":"abc123","aliases":[{"alias":"tierbridge/auto","profile":"free-first","candidates":[` + candidates + `]}]}`
}

// chatBody builds an OpenAI chat body with text of the given size; extra is spliced in as top-level JSON.
func chatBody(textBytes int, extra string) string {
	return `{"model":"tierbridge/auto","messages":[{"role":"user","content":"` + strings.Repeat("a", textBytes) + `"}]` + extra + `}`
}

const (
	toolsExtra = `,"tools":[{"type":"function","function":{"name":"f","parameters":{"type":"object"}}}]`
	imageBody  = `{"messages":[{"role":"user","content":[{"type":"text","text":"hi"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}]}`
)

// hostRouteRequest serialises a model.route request the way the host does: the Go field names of
// pluginapi.ModelRouteRequest in declaration order, the body as base64, plus host_callback_id.
func hostRouteRequest(t *testing.T, model, body string, providers []string) []byte {
	t.Helper()
	type request struct {
		Plugin             map[string]any
		PluginID           string
		SourceFormat       string
		RequestedModel     string
		Stream             bool
		Headers            map[string][]string
		Query              map[string][]string
		Body               []byte
		Metadata           map[string]any
		AvailableProviders []string
		HostCallbackID     string `json:"host_callback_id,omitempty"`
	}
	raw, err := wire.Marshal(request{
		Plugin:             map[string]any{"Name": "tierbridge", "ConfigFields": []any{}},
		PluginID:           "tierbridge",
		SourceFormat:       "openai",
		RequestedModel:     model,
		Headers:            map[string][]string{"Content-Type": {"application/json"}},
		Query:              map[string][]string{},
		Body:               []byte(body),
		Metadata:           map[string]any{"requested_model": model},
		AvailableProviders: providers,
		HostCallbackID:     "cb-1",
	})
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func lifecycle(t *testing.T, yaml string) []byte {
	t.Helper()
	raw, err := wire.Marshal(wire.LifecycleRequest{ConfigYAML: []byte(yaml), SchemaVersion: wire.SchemaVersion, MultiAuthProviders: true})
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func decode(t *testing.T, raw []byte, result any) wire.Envelope {
	t.Helper()
	var env wire.Envelope
	if err := json.Unmarshal(raw, &env); err != nil {
		t.Fatalf("envelope %s: %v", raw, err)
	}
	if result != nil && env.OK {
		if err := json.Unmarshal(env.Result, result); err != nil {
			t.Fatalf("result %s: %v", env.Result, err)
		}
	}
	return env
}

func writeFile(t *testing.T, path, data string, mtime time.Time) {
	t.Helper()
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(path, mtime, mtime); err != nil {
		t.Fatal(err)
	}
}

// newPlugin registers a handler configured with a router file holding router (when non-empty).
func newPlugin(t *testing.T, router, extraYAML string) (*Handler, string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "tierbridge-router.json")
	if router != "" {
		writeFile(t, path, router, clock)
	}
	h := NewHandler()
	h.now = func() time.Time { return clock }
	t.Cleanup(h.persist.Close) // before the temp dir goes: a late write must not recreate it
	yaml := fmt.Sprintf("enabled: true\npriority: 0\nrouter-file: %q\n%s", path, extraYAML)
	if env := decode(t, h.Handle(wire.MethodPluginRegister, lifecycle(t, yaml)), nil); !env.OK {
		t.Fatalf("register failed: %+v", env.Error)
	}
	return h, path
}

func route(t *testing.T, h *Handler, model, body string, providers []string) wire.ModelRouteResponse {
	t.Helper()
	var resp wire.ModelRouteResponse
	env := decode(t, h.Handle(wire.MethodModelRoute, hostRouteRequest(t, model, body, providers)), &resp)
	if !env.OK {
		t.Fatalf("model.route returned an error envelope: %+v", env.Error)
	}
	return resp
}

func TestRoute(t *testing.T) {
	tests := []struct {
		name       string
		model      string
		body       string
		providers  []string
		wantTarget string // "" = Handled:false
		wantModel  string
		wantReason string // prefix
	}{
		{name: "non-alias model passes through", model: "gpt-4o", body: chatBody(40, ""), wantTarget: ""},
		{name: "empty requested model passes through", model: "", body: chatBody(40, ""), wantTarget: ""},
		{name: "alias with a suffix is not the alias", model: "tierbridge/auto-2", body: chatBody(40, ""), wantTarget: ""},
		{name: "ranking order: first candidate wins", model: aliasModel, body: chatBody(40, ""), wantTarget: "small", wantModel: "s", wantReason: "rank 1 of 4"},
		{name: "alias match ignores case", model: "TierBridge/Auto", body: chatBody(40, ""), wantTarget: "small", wantModel: "s", wantReason: "rank 1 of 4"},
		{name: "context window skip", model: aliasModel, body: chatBody(4000, ""), wantTarget: "notools", wantModel: "n", wantReason: "rank 2 of 4 (skipped 1: small/s context)"},
		{name: "tools skip", model: aliasModel, body: chatBody(4000, toolsExtra), wantTarget: "textonly", wantModel: "t", wantReason: "rank 3 of 4 (skipped 2: small/s context, notools/n tools)"},
		{name: "image skip", model: aliasModel, body: imageBodyWithTools(), wantTarget: "big", wantModel: "b", wantReason: "rank 4 of 4 (skipped 3: small/s context, notools/n tools, textonly/t image)"},
		{name: "image alone fits an image model", model: aliasModel, body: imageBody, wantTarget: "small", wantModel: "s", wantReason: "rank 1 of 4"},
		{name: "provider not available skip", model: aliasModel, body: chatBody(40, ""), providers: []string{"notools", "textonly"}, wantTarget: "notools", wantModel: "n", wantReason: "rank 2 of 4 (skipped 1: small/s provider)"},
		{name: "no candidate fits", model: aliasModel, body: chatBody(40, ""), providers: []string{"somebody-else"}, wantTarget: "", wantReason: "no candidate fits (small/s provider, notools/n provider, textonly/t provider, big/b provider)"},
		{name: "request larger than every window", model: aliasModel, body: chatBody(5_000_000, ""), wantTarget: "", wantReason: "no candidate fits (small/s context, notools/n context, textonly/t context, big/b context)"},
		{name: "unparseable body still routes by size", model: aliasModel, body: "not json", wantTarget: "small", wantModel: "s", wantReason: "rank 1 of 4"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h, _ := newPlugin(t, rankingJSON, "")
			got := route(t, h, tc.model, tc.body, tc.providers)
			if (got.Target != "") != (tc.wantTarget != "") || got.Handled != (tc.wantTarget != "") {
				t.Fatalf("response = %+v, want Handled=%v", got, tc.wantTarget != "")
			}
			if got.Handled {
				if got.TargetKind != wire.ModelRouteTargetProvider || got.Target != tc.wantTarget || got.TargetModel != tc.wantModel {
					t.Errorf("response = %+v, want provider %s/%s", got, tc.wantTarget, tc.wantModel)
				}
			} else if got.TargetKind != "" || got.Target != "" || got.TargetModel != "" {
				t.Errorf("unhandled response carries a target: %+v", got)
			}
			if got.Reason != tc.wantReason {
				t.Errorf("Reason = %q, want %q", got.Reason, tc.wantReason)
			}
		})
	}
}

func imageBodyWithTools() string {
	return `{"messages":[{"role":"user","content":[{"type":"text","text":"` + strings.Repeat("a", 4000) + `"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}]` + toolsExtra + `}`
}

func TestRouteRecordsDecisionsOnlyForTheAlias(t *testing.T) {
	h, _ := newPlugin(t, rankingJSON, "decisions-keep: 2\n")
	route(t, h, "gpt-4o", chatBody(40, ""), nil)
	if n := len(h.Status().Decisions); n != 0 {
		t.Fatalf("non-alias request recorded %d decisions", n)
	}
	route(t, h, aliasModel, chatBody(40, ""), nil)
	route(t, h, aliasModel, chatBody(4000, ""), nil)
	route(t, h, aliasModel, chatBody(4000, toolsExtra), nil)
	got := h.Status().Decisions
	if len(got) != 2 {
		t.Fatalf("decisions-keep 2 kept %d", len(got))
	}
	newest, older := got[0], got[1]
	if !newest.Handled || newest.Rank != 3 || newest.Provider != "textonly" || !newest.Tools || newest.EstTokens < 1000 || len(newest.Skips) != 2 {
		t.Errorf("newest decision = %+v", newest)
	}
	if older.Rank != 2 || older.Provider != "notools" || older.Of != 4 || !older.Time.Equal(clock) {
		t.Errorf("older decision = %+v", older)
	}
}

func TestRouteRPMOneSkipsTheCandidateOnTheSecondRequest(t *testing.T) {
	limited := wrap(`{"provider":"free","model":"f","quota":{"rpm":1}},
	 {"provider":"paid","model":"p"}`)
	h, _ := newPlugin(t, limited, "")
	first := route(t, h, aliasModel, chatBody(40, ""), nil)
	second := route(t, h, aliasModel, chatBody(40, ""), nil)
	if first.Target != "free" || second.Target != "paid" {
		t.Fatalf("targets = %q then %q, want free then paid", first.Target, second.Target)
	}
	if second.Reason != "rank 2 of 2 (skipped 1: free/f rpm)" {
		t.Errorf("second Reason = %q", second.Reason)
	}
	// Once the minute has passed the first candidate is available again.
	h.now = func() time.Time { return clock.Add(2 * time.Minute) }
	if third := route(t, h, aliasModel, chatBody(40, ""), nil); third.Target != "free" {
		t.Errorf("after the window target = %q, want free", third.Target)
	}
}

func TestRouteDailyBudgetExceeded(t *testing.T) {
	priced := wrap(`{"provider":"paid","model":"p","cost":{"status":"known","input":1,"output":2}}`)
	h, _ := newPlugin(t, priced, "daily-budget-usd: 1\n")
	// 1M input tokens at $1/M and 1M output tokens at $2/M: $3 spent, over the $1 budget.
	h.Handle(wire.MethodUsageHandle, []byte(`{"Model":"p","Detail":{"InputTokens":1000000,"OutputTokens":1000000}}`))
	got := route(t, h, aliasModel, chatBody(40, ""), nil)
	if got.Handled || !strings.HasPrefix(got.Reason, "no candidate fits (paid/p budget") {
		t.Errorf("response = %+v, want Handled:false with budget skips", got)
	}
}

func TestRouteWithoutUsableRouterFile(t *testing.T) {
	t.Run("no router-file configured", func(t *testing.T) {
		h := NewHandler()
		h.Handle(wire.MethodPluginRegister, lifecycle(t, "enabled: true\n"))
		got := route(t, h, aliasModel, chatBody(40, ""), nil)
		if got.Handled || got.Reason != "router file: "+"no router-file configured" {
			t.Errorf("response = %+v", got)
		}
	})
	t.Run("missing file", func(t *testing.T) {
		h, _ := newPlugin(t, "", "")
		got := route(t, h, aliasModel, chatBody(40, ""), nil)
		if got.Handled || !strings.HasPrefix(got.Reason, "router file: ") {
			t.Errorf("response = %+v", got)
		}
		if h.Status().Router.Error == "" {
			t.Errorf("Status().Router.Error is empty for a missing file")
		}
	})
	t.Run("invalid first version", func(t *testing.T) {
		h, _ := newPlugin(t, `{"schema":"tierbridge-router/1","aliases":[`, "")
		got := route(t, h, aliasModel, chatBody(40, ""), nil)
		if got.Handled {
			t.Errorf("response = %+v, want Handled:false", got)
		}
	})
}

func TestInvalidRouterFileKeepsThePreviousVersion(t *testing.T) {
	h, path := newPlugin(t, rankingJSON, "")
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.Target != "small" {
		t.Fatalf("baseline target = %q", got.Target)
	}

	// A valid new version takes effect on the next call.
	writeFile(t, path, wrap(`{"provider":"fresh","model":"f"}`), clock.Add(time.Minute))
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.Target != "fresh" {
		t.Fatalf("reloaded target = %q, want fresh", got.Target)
	}

	// An invalid one is rejected: the previous ranking keeps serving and the error is kept.
	writeFile(t, path, wrap(`{"provider":"broken"}`), clock.Add(2*time.Minute))
	got := route(t, h, aliasModel, chatBody(40, ""), nil)
	if !got.Handled || got.Target != "fresh" {
		t.Fatalf("after an invalid file response = %+v, want the previous ranking", got)
	}
	st := h.Status()
	if !strings.Contains(st.Router.Error, "provider and model are required") || len(st.Router.Aliases) != 1 || st.Router.Aliases[0].Candidates != 1 {
		t.Errorf("Status().Router = %+v", st.Router)
	}

	// Fixing the file clears the error.
	writeFile(t, path, rankingJSON, clock.Add(3*time.Minute))
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.Target != "small" {
		t.Errorf("target after the fix = %q, want small", got.Target)
	}
	if e := h.Status().Router.Error; e != "" {
		t.Errorf("Router.Error = %q after the fix", e)
	}
}

func TestDebugForceErrorFallsThrough(t *testing.T) {
	h, _ := newPlugin(t, rankingJSON, "debug-force-error: true\n")
	got := route(t, h, aliasModel, chatBody(40, ""), nil)
	if got.Handled || got.Target != "" {
		t.Fatalf("response = %+v, want Handled:false", got)
	}
	st := h.Status()
	if !strings.Contains(st.LastError, "panic") || st.LastErrorAt.IsZero() {
		t.Errorf("LastError = %q at %v, want the recovered panic", st.LastError, st.LastErrorAt)
	}
	if len(st.Decisions) != 1 || st.Decisions[0].Handled || st.Decisions[0].Reason != "internal error" {
		t.Errorf("decisions = %+v", st.Decisions)
	}
	// Other models are untouched by the switch.
	if got := route(t, h, "gpt-4o", chatBody(40, ""), nil); got.Handled || got.Reason != "" {
		t.Errorf("non-alias response = %+v", got)
	}
	// Turning the switch off restores routing.
	cfg, _ := h.config()
	path := cfg.RouterFile
	h.Handle(wire.MethodPluginReconfigure, lifecycle(t, fmt.Sprintf("router-file: %q\n", path)))
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); !got.Handled {
		t.Errorf("after reconfigure response = %+v, want Handled", got)
	}
}

func TestMalformedRouteRequestIsNotHandled(t *testing.T) {
	h, _ := newPlugin(t, rankingJSON, "")
	for name, raw := range map[string]string{
		"empty":          ``,
		"not json":       `nope`,
		"not an object":  `[1,2]`,
		"truncated":      `{"RequestedModel":"tierbridge/auto","Body":`,
		"body not bytes": `{"RequestedModel":"tierbridge/auto","Body":{"a":1}}`,
	} {
		t.Run(name, func(t *testing.T) {
			var resp wire.ModelRouteResponse
			env := decode(t, h.Handle(wire.MethodModelRoute, []byte(raw)), &resp)
			if !env.OK || resp.Handled {
				t.Errorf("envelope %+v response %+v, want ok with Handled:false", env, resp)
			}
		})
	}
}

func TestPeekRequestedModel(t *testing.T) {
	tests := []struct{ name, raw, want string }{
		{"first", `{"RequestedModel":"a","Body":"x"}`, "a"},
		{"after other fields", `{"Plugin":{"Name":"x","ConfigFields":[1,2]},"PluginID":"p","RequestedModel":" b ","Body":"x"}`, "b"},
		{"after the body", `{"Body":"eHh4","RequestedModel":"c"}`, "c"},
		{"absent", `{"Body":"x"}`, ""},
		{"not an object", `"RequestedModel"`, ""},
		{"truncated", `{"Plugin":{`, ""},
		{"wrong type", `{"RequestedModel":5}`, ""},
	}
	for _, tc := range tests {
		if got := peekRequestedModel([]byte(tc.raw)); got != tc.want {
			t.Errorf("%s: peekRequestedModel(%s) = %q, want %q", tc.name, tc.raw, got, tc.want)
		}
	}
}

func TestReconfigure(t *testing.T) {
	h, path := newPlugin(t, rankingJSON, "")
	status := func() Status { return h.Status() }

	// A config that does not parse keeps the previous one and surfaces the error.
	env := decode(t, h.Handle(wire.MethodPluginReconfigure, lifecycle(t, "cooldown-seconds: soon\n")), nil)
	if !env.OK {
		t.Fatalf("reconfigure with a bad config must still succeed: %+v", env.Error)
	}
	if st := status(); st.ConfigError == "" || st.Alias != aliasModel {
		t.Errorf("Status() = %+v", st)
	}
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.Target != "small" {
		t.Errorf("routing broke after a bad reconfigure: %+v", got)
	}

	// A good config replaces it, including the alias.
	h.Handle(wire.MethodPluginReconfigure, lifecycle(t, fmt.Sprintf("router-file: %q\nalias: my/auto\n", path)))
	st := status()
	if st.ConfigError != "" || st.Alias != "my/auto" {
		t.Errorf("Status() = %+v", st)
	}
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.Handled {
		t.Errorf("old alias still handled: %+v", got)
	}
	// The renamed alias is answered by the plugin, but the router file must define it.
	if got := route(t, h, "my/auto", chatBody(40, ""), nil); got.Handled || got.Reason != `router file has no alias "my/auto"` {
		t.Errorf("new alias without an entry = %+v, want Handled:false", got)
	}
	writeFile(t, path, strings.Replace(rankingJSON, `"alias":"tierbridge/auto"`, `"alias":"my/auto"`, 1), clock.Add(time.Hour))
	if got := route(t, h, "my/auto", chatBody(40, ""), nil); !got.Handled || got.Target != "small" {
		t.Errorf("new alias with an entry = %+v, want small/s", got)
	}
}

func TestOtherMethods(t *testing.T) {
	h := NewHandler()
	for _, method := range []string{wire.MethodPluginQuiesce, wire.MethodPluginShutdown, wire.MethodUsageHandle} {
		if got := string(h.Handle(method, []byte(`{}`))); got != `{"ok":true,"result":{}}` {
			t.Errorf("%s = %s, want an empty success", method, got)
		}
	}
	var reg wire.ManagementRegistrationResponse
	decode(t, h.Handle(wire.MethodManagementRegister, nil), &reg)
	if len(reg.Routes) != 5 || len(reg.Resources) != 0 {
		t.Errorf("management.register = %+v", reg)
	}
	var resp wire.ManagementResponse
	if env := decode(t, h.Handle(wire.MethodManagementHandle, nil), &resp); !env.OK || resp.StatusCode != 404 {
		t.Errorf("management.handle = %+v %+v", env, resp)
	}
	env := decode(t, h.Handle("no.such.method", nil), nil)
	if env.OK || env.Error == nil || env.Error.Code != wire.ErrorCodeUnknownMethod {
		t.Errorf("unknown method = %+v", env)
	}
	env = decode(t, h.Handle(wire.MethodPluginRegister, []byte(`{not json`)), nil)
	if env.OK || env.Error == nil || env.Error.Code != wire.ErrorCodeBadRequest {
		t.Errorf("bad lifecycle request = %+v", env)
	}
}

func TestHandlePanicBecomesAnErrorEnvelope(t *testing.T) {
	h := NewHandler()
	h.acct = nil // makes applyConfig dereference nil
	env := decode(t, h.Handle(wire.MethodPluginRegister, lifecycle(t, "alias: x\n")), nil)
	if env.OK || env.Error == nil || env.Error.Code != wire.ErrorCodeInternal {
		t.Fatalf("envelope = %+v, want an internal_error", env)
	}
	if !strings.Contains(h.Status().LastError, "panic") {
		t.Errorf("the panic was not kept as the last error")
	}
}

// Golden JSON: the exact bytes the host will decode. Any change here is a wire contract change and
// must be checked against sdk/pluginabi and sdk/pluginapi of the pinned host (see package wire).
func TestGoldenRegister(t *testing.T) {
	const golden = `{"ok":true,"result":{"schema_version":10,"metadata":{"Name":"tierbridge","Version":"0.1.0","Author":"blyzer","GitHubRepository":"https://github.com/blyzer/llm-tierbridge","Logo":"","ConfigFields":[` +
		`{"Name":"router-file","Type":"string","Default":"","EnumValues":null,"Description":"Path of tierbridge-router.json, the ranking exported by 'tierbridge export cpa-router'. Reloaded when the file changes; empty means the alias is never handled."},` +
		`{"Name":"alias","Type":"string","Default":"tierbridge/auto","EnumValues":null,"Description":"Model name the plugin answers to. Every other model is left to the host."},` +
		`{"Name":"daily-budget-usd","Type":"number","Default":0,"EnumValues":null,"Description":"Estimated spend allowed per UTC day, in USD. 0 means no budget."},` +
		`{"Name":"cooldown-seconds","Type":"integer","Default":60,"EnumValues":null,"Description":"Seconds a candidate is skipped after an upstream failure reported by the usage plugin."},` +
		`{"Name":"state-path","Type":"string","Default":"","EnumValues":null,"Description":"File the quota counters, spend and cooldowns persist to across restarts (atomic JSON, mode 0600, written at most every 5 s). Empty means no persistence."},` +
		`{"Name":"decisions-keep","Type":"integer","Default":50,"EnumValues":null,"Description":"How many recent routing decisions the management status keeps."},` +
		`{"Name":"debug-force-error","Type":"boolean","Default":false,"EnumValues":null,"Description":"TEST SWITCH ONLY: makes model routing fail internally so an acceptance run can prove the host falls through."}` +
		`]},"capabilities":{"model_router":true,"usage_plugin":true,"management_api":true}}}`

	hostConfig := "enabled: true\npriority: 0\n"
	for _, method := range []string{wire.MethodPluginRegister, wire.MethodPluginReconfigure} {
		got := string(NewHandler().Handle(method, lifecycle(t, hostConfig)))
		if got != golden {
			t.Errorf("%s response:\n got %s\nwant %s", method, got, golden)
		}
	}
	if wire.SchemaVersion != 10 || wire.ABIVersion != 1 {
		t.Errorf("pin drifted: ABI %d schema %d, want 1 and 10 (CLIProxyAPI 47f18e4d)", wire.ABIVersion, wire.SchemaVersion)
	}
}

func TestGoldenLifecycleRequest(t *testing.T) {
	// config_yaml is base64 on the wire: "enabled: true\n" -> ZW5hYmxlZDogdHJ1ZQo=
	const hostJSON = `{"config_yaml":"ZW5hYmxlZDogdHJ1ZQo=","schema_version":10,"multi_auth_providers":true}`
	var req wire.LifecycleRequest
	if err := json.Unmarshal([]byte(hostJSON), &req); err != nil {
		t.Fatal(err)
	}
	if string(req.ConfigYAML) != "enabled: true\n" || req.SchemaVersion != 10 || !req.MultiAuthProviders {
		t.Fatalf("decoded %+v", req)
	}
	if got, _ := wire.Marshal(req); string(got) != hostJSON {
		t.Errorf("re-encoded %s, want %s", got, hostJSON)
	}
}

func TestGoldenRoute(t *testing.T) {
	h, _ := newPlugin(t, rankingJSON, "")

	handled := string(h.Handle(wire.MethodModelRoute, hostRouteRequest(t, aliasModel, chatBody(4000, ""), []string{"notools", "big"})))
	const wantHandled = `{"ok":true,"result":{"Handled":true,"TargetKind":"provider","Target":"notools","TargetModel":"n","Reason":"rank 2 of 4 (skipped 1: small/s context)"}}`
	if handled != wantHandled {
		t.Errorf("handled route:\n got %s\nwant %s", handled, wantHandled)
	}

	passthrough := string(h.Handle(wire.MethodModelRoute, hostRouteRequest(t, "gpt-4o", chatBody(40, ""), nil)))
	const wantPassthrough = `{"ok":true,"result":{"Handled":false,"TargetKind":"","Target":"","TargetModel":"","Reason":""}}`
	if passthrough != wantPassthrough {
		t.Errorf("passthrough route:\n got %s\nwant %s", passthrough, wantPassthrough)
	}

	nofit := string(h.Handle(wire.MethodModelRoute, hostRouteRequest(t, aliasModel, chatBody(40, ""), []string{"x"})))
	const wantNoFit = `{"ok":true,"result":{"Handled":false,"TargetKind":"","Target":"","TargetModel":"","Reason":"no candidate fits (small/s provider, notools/n provider, textonly/t provider, big/b provider)"}}`
	if nofit != wantNoFit {
		t.Errorf("no-fit route:\n got %s\nwant %s", nofit, wantNoFit)
	}
}

func TestGoldenErrorEnvelope(t *testing.T) {
	got := string(NewHandler().Handle("nope", nil))
	const want = `{"ok":false,"error":{"code":"unknown_method","message":"unknown method: nope"}}`
	if got != want {
		t.Errorf("got %s, want %s", got, want)
	}
}

func TestAliasEntryInTheRouterFile(t *testing.T) {
	multi := `{"schema":"tierbridge-router/1","catalogSha256":"c1","aliases":[
	 {"alias":"tierbridge/empty","profile":"free-first","candidates":[]},
	 {"alias":"tierbridge/auto","profile":"quality","candidates":[{"provider":"p","model":"m","id":"upstream","extra":true}]}]}`
	h, _ := newPlugin(t, multi, "")
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); !got.Handled || got.Target != "p" || got.TargetModel != "m" {
		t.Errorf("response = %+v, want p/m from the matching alias entry", got)
	}

	// An alias with no candidates is valid and falls through with a Reason.
	h.Handle(wire.MethodPluginReconfigure, lifecycle(t, h.routerFileYAML()+"alias: tierbridge/empty\n"))
	got := route(t, h, "tierbridge/empty", chatBody(40, ""), nil)
	if got.Handled || got.Reason != "no candidates" {
		t.Errorf("empty alias response = %+v, want Handled:false and Reason \"no candidates\"", got)
	}

	// A configured alias the file does not define falls through and says so.
	h.Handle(wire.MethodPluginReconfigure, lifecycle(t, h.routerFileYAML()+"alias: tierbridge/other\n"))
	got = route(t, h, "tierbridge/other", chatBody(40, ""), nil)
	if got.Handled || got.Reason != `router file has no alias "tierbridge/other"` {
		t.Errorf("missing alias response = %+v", got)
	}
}

func (h *Handler) routerFileYAML() string {
	cfg, _ := h.config()
	return fmt.Sprintf("router-file: %q\n", cfg.RouterFile)
}

func TestStatusRevisionAndUnpriced(t *testing.T) {
	h, _ := newPlugin(t, wrap(`{"provider":"p","model":"m","cost":{"status":"unknown","input":null,"output":null}},{"provider":"q","model":"n","cost":{"status":"free","input":0,"output":0}}`), "")
	got := route(t, h, aliasModel, chatBody(40, ""), nil)
	if !got.Handled || got.Target != "p" {
		t.Fatalf("response = %+v", got)
	}
	st := h.Status()
	if st.Router.Revision.CatalogSHA256 != "abc123" || len(st.Router.Revision.FileSHA256) != 64 {
		t.Errorf("revision = %+v", st.Router.Revision)
	}
	if len(st.Router.Aliases) != 1 || st.Router.Aliases[0].Unpriced != 1 || st.Router.Aliases[0].Profile != "free-first" {
		t.Errorf("aliases = %+v, want 1 unpriced flagged", st.Router.Aliases)
	}
	if len(st.Decisions) != 1 || !st.Decisions[0].Unpriced {
		t.Errorf("decisions = %+v, want the unpriced pick flagged", st.Decisions)
	}
}
