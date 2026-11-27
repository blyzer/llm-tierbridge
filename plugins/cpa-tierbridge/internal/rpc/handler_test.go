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

const rankingJSON = `{"schema":"tierbridge-router/2","target":"host","modelPrefix":"","catalogSha256":"abc123","aliases":[{"alias":"tierbridge/auto","profile":"free-first","candidates":[
 {"model":"s","contextWindow":100,"supportsTools":true,"input":["text","image"]},
 {"model":"n","contextWindow":100000,"supportsTools":false,"input":["text","image"]},
 {"model":"t","contextWindow":100000,"supportsTools":true,"input":["text"]},
 {"model":"b","contextWindow":1000000,"supportsTools":true,"input":["text","image"]}
]}]}`

const aliasModel = "tierbridge/auto"

// wrap puts candidate JSON into a valid router file holding the default alias.
func wrap(candidates string) string {
	return `{"schema":"tierbridge-router/2","target":"host","modelPrefix":"","catalogSha256":"abc123","aliases":[{"alias":"tierbridge/auto","profile":"free-first","candidates":[` + candidates + `]}]}`
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
	t.Cleanup(h.Flush) // before the temp dir goes: a late write must not recreate it
	yaml := fmt.Sprintf("enabled: true\npriority: 0\nrouter-file: %q\n%s", path, extraYAML)
	if env := decode(t, h.Handle(wire.MethodPluginRegister, lifecycle(t, yaml)), nil); !env.OK {
		t.Fatalf("register failed: %+v", env.Error)
	}
	return h, path
}

func imageBodyWithTools() string {
	return `{"messages":[{"role":"user","content":[{"type":"text","text":"` + strings.Repeat("a", 4000) + `"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}]` + toolsExtra + `}`
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
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.TargetModel != "s" {
		t.Fatalf("baseline target = %q", got.Target)
	}

	// A valid new version takes effect on the next call.
	writeFile(t, path, wrap(`{"model":"f"}`), clock.Add(time.Minute))
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.TargetModel != "f" {
		t.Fatalf("reloaded target = %q, want fresh", got.Target)
	}

	// An invalid one is rejected: the previous ranking keeps serving and the error is kept.
	writeFile(t, path, wrap(`{"id":"broken"}`), clock.Add(2*time.Minute))
	got := route(t, h, aliasModel, chatBody(40, ""), nil)
	if !got.Handled || got.TargetModel != "f" {
		t.Fatalf("after an invalid file response = %+v, want the previous ranking", got)
	}
	st := h.Status()
	if !strings.Contains(st.Router.Error, "model is required") || len(st.Router.Aliases) != 1 || st.Router.Aliases[0].Candidates != 1 {
		t.Errorf("Status().Router = %+v", st.Router)
	}

	// Fixing the file clears the error.
	writeFile(t, path, rankingJSON, clock.Add(3*time.Minute))
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.TargetModel != "s" {
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
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.TargetModel != "s" {
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
	if got := route(t, h, "my/auto", chatBody(40, ""), nil); !got.Handled || got.TargetModel != "s" {
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

func TestGoldenErrorEnvelope(t *testing.T) {
	got := string(NewHandler().Handle("nope", nil))
	const want = `{"ok":false,"error":{"code":"unknown_method","message":"unknown method: nope"}}`
	if got != want {
		t.Errorf("got %s, want %s", got, want)
	}
}

func TestAliasEntryInTheRouterFile(t *testing.T) {
	multi := `{"schema":"tierbridge-router/2","target":"host","catalogSha256":"c1","aliases":[
	 {"alias":"tierbridge/empty","profile":"free-first","candidates":[]},
	 {"alias":"tierbridge/auto","profile":"quality","candidates":[{"model":"m","id":"upstream","extra":true}]}]}`
	h, _ := newPlugin(t, multi, "")
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); !got.Handled || got.TargetModel != "m" {
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
	h, _ := newPlugin(t, wrap(`{"model":"m","cost":{"status":"unknown","input":null,"output":null}},{"model":"n","cost":{"status":"free","input":0,"output":0}}`), "")
	got := route(t, h, aliasModel, chatBody(40, ""), nil)
	if !got.Handled || got.TargetModel != "m" {
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
