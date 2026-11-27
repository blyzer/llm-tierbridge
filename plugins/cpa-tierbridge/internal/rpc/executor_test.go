package rpc

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

// fakeHost is the injected HostCaller: it records every host.model.execute request and answers with
// whatever handle returns, so the executor is tested without cgo and without a host.
type fakeHost struct {
	mu     sync.Mutex
	reqs   []wire.HostModelRequest
	raw    [][]byte
	handle func(req wire.HostModelRequest) []byte
}

func (f *fakeHost) Call(method string, request []byte) ([]byte, error) {
	if method != wire.MethodHostModelExecute {
		return nil, fmt.Errorf("unexpected host method %q", method)
	}
	var req wire.HostModelRequest
	if err := json.Unmarshal(request, &req); err != nil {
		return nil, err
	}
	f.mu.Lock()
	f.reqs = append(f.reqs, req)
	f.raw = append(f.raw, append([]byte(nil), request...))
	f.mu.Unlock()
	return f.handle(req), nil
}

func (f *fakeHost) models() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	out := make([]string, len(f.reqs))
	for i, r := range f.reqs {
		out[i] = r.Model
	}
	return out
}

// hostOK is the envelope the host answers a host.model.execute with (the upstream status inside).
func hostOK(status int, body string, headers map[string][]string) []byte {
	raw, _ := wire.OKEnvelope(wire.HostModelResponse{StatusCode: status, Headers: headers, Body: []byte(body)})
	return raw
}

// hostFail is the envelope the host answers with when the call itself failed (host_call_failed).
func hostFail(status int, msg string) []byte {
	return wire.ErrorEnvelopeStatus("host_call_failed", msg, status, 0)
}

// byModel builds a handle that answers per candidate model; unknown models answer 200.
func byModel(m map[string][]byte) func(wire.HostModelRequest) []byte {
	return func(req wire.HostModelRequest) []byte {
		if raw, ok := m[req.Model]; ok {
			return raw
		}
		return hostOK(200, `{"from":"`+req.Model+`"}`, map[string][]string{"Content-Type": {"application/json"}})
	}
}

const abc = `{"model":"tb-a","id":"a","cost":{"status":"free"}},{"model":"tb-b","id":"b","cost":{"status":"free"}},{"model":"tb-c","id":"c","cost":{"status":"free"}}`

// execRequest serialises an executor.execute request the way the host does.
func execRequest(t *testing.T, body string) []byte {
	t.Helper()
	raw, err := wire.Marshal(wire.ExecutorRequest{
		Model: "tierbridge/auto", Format: "openai", SourceFormat: "openai",
		Headers:         map[string][]string{"Content-Type": {"application/json"}, "Authorization": {"Bearer k"}, "Content-Length": {"99"}},
		Query:           map[string][]string{},
		OriginalRequest: []byte(body), Payload: []byte(body), HostCallbackID: "cb-1",
	})
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func execute(t *testing.T, h *Handler, body string) (wire.Envelope, wire.ExecutorResponse) {
	t.Helper()
	var resp wire.ExecutorResponse
	env := decode(t, h.Handle(wire.MethodExecutorExecute, execRequest(t, body)), &resp)
	return env, resp
}

func newExecutor(t *testing.T, router, yaml string, fh *fakeHost) *Handler {
	t.Helper()
	h, _ := newPlugin(t, router, yaml)
	h.SetHost(fh)
	return h
}

// route is model.route followed, when the plugin took the request, by executor.execute against a host
// that answers 200 for every model, the way the host runs a TargetKind "self" decision. TargetModel is the
// candidate that answered and Reason the reason of the decision the execution recorded.
func route(t *testing.T, h *Handler, model, body string, providers []string) wire.ModelRouteResponse {
	t.Helper()
	var resp wire.ModelRouteResponse
	env := decode(t, h.Handle(wire.MethodModelRoute, hostRouteRequest(t, model, body, providers)), &resp)
	if !env.OK {
		t.Fatalf("model.route returned an error envelope: %+v", env.Error)
	}
	if !resp.Handled {
		return resp
	}
	if resp.TargetKind != wire.ModelRouteTargetSelf {
		t.Fatalf("TargetKind = %q, want self", resp.TargetKind)
	}
	fh := &fakeHost{handle: byModel(nil)}
	h.SetHost(fh)
	ex, _ := execute(t, h, body)
	if d := h.Status().Decisions; len(d) > 0 {
		resp.Reason = d[0].Reason
	}
	if !ex.OK {
		resp.Handled = false
		return resp
	}
	resp.TargetModel = fh.models()[len(fh.models())-1]
	return resp
}

func lastDecision(t *testing.T, h *Handler) (d struct {
	Attempts []string
	Skips    []string
	Reason   string
	Status   int
	Model    string
}) {
	t.Helper()
	ds := h.Status().Decisions
	if len(ds) == 0 {
		t.Fatal("no decision recorded")
	}
	for _, a := range ds[0].Attempts {
		d.Attempts = append(d.Attempts, fmt.Sprintf("%s:%d:ok=%v:fallback=%v:cool=%v", a.Model, a.Status, a.OK, a.Fallback, a.CoolingDown))
	}
	for _, s := range ds[0].Skips {
		d.Skips = append(d.Skips, fmt.Sprintf("%s:%s", s.Model, s.Reason))
	}
	d.Reason, d.Status, d.Model = ds[0].Reason, ds[0].Status, ds[0].Model
	return d
}

func TestRouteTakesTheAliasWithTheSelfExecutor(t *testing.T) {
	h, _ := newPlugin(t, wrap(abc), "")
	var got wire.ModelRouteResponse
	decode(t, h.Handle(wire.MethodModelRoute, hostRouteRequest(t, "tierbridge/auto", chatBody(10, ""), []string{"gemini"})), &got)
	if !got.Handled || got.TargetKind != wire.ModelRouteTargetSelf || got.Target != "" || got.TargetModel != "" || got.Reason != "rank 1 of 3" {
		t.Errorf("route = %+v", got)
	}
	if n := len(h.Status().Decisions); n != 0 {
		t.Errorf("a served request is recorded by the executor, route recorded %d decisions", n)
	}
	// Route only previews: no quota is consumed until the executor runs.
	if c := h.acct.Counters(nil, clock); len(c) != 0 {
		t.Errorf("counters = %+v", c)
	}
}

func TestExecuteFallsBackOnTheFirstFailureInOneCall(t *testing.T) {
	fh := &fakeHost{handle: byModel(map[string][]byte{"tb-a": hostOK(503, `{"error":"overloaded"}`, nil)})}
	h := newExecutor(t, wrap(abc), "", fh)
	env, resp := execute(t, h, chatBody(10, ""))
	if !env.OK || string(resp.Payload) != `{"from":"tb-b"}` {
		t.Fatalf("execute = %+v / %s", env, resp.Payload)
	}
	if got := strings.Join(fh.models(), ","); got != "tb-a,tb-b" {
		t.Errorf("host saw %s, want tb-a,tb-b in one call", got)
	}
	d := lastDecision(t, h)
	want := []string{"tb-a:503:ok=false:fallback=true:cool=true", "tb-b:200:ok=true:fallback=false:cool=false"}
	if strings.Join(d.Attempts, "|") != strings.Join(want, "|") || d.Model != "tb-b" || d.Status != 200 {
		t.Errorf("decision = %+v, want attempts %v", d, want)
	}
	// What the host receives: the client protocol both ways, the candidate as model (also inside the
	// body), the callback id, and no hop-by-hop headers.
	req := fh.reqs[1]
	if req.EntryProtocol != "openai" || req.ExitProtocol != "openai" || req.Stream || req.HostCallbackID != "cb-1" {
		t.Errorf("host request = %+v", req)
	}
	if !strings.Contains(string(req.Body), `"model":"tb-b"`) || strings.Contains(string(req.Body), "tierbridge/auto") {
		t.Errorf("host request body = %s", req.Body)
	}
	if _, ok := req.Headers["Content-Length"]; ok || req.Headers["Authorization"][0] != "Bearer k" {
		t.Errorf("host request headers = %+v", req.Headers)
	}
}

func TestExecute429CoolsTheCandidateForTheNextRequest(t *testing.T) {
	fh := &fakeHost{handle: byModel(map[string][]byte{"tb-a": hostOK(429, `{"error":"slow down"}`, map[string][]string{"Retry-After": {"30"}})})}
	h := newExecutor(t, wrap(abc), "", fh)
	if env, _ := execute(t, h, chatBody(10, "")); !env.OK {
		t.Fatalf("first request: %+v", env.Error)
	}
	if got := strings.Join(fh.models(), ","); got != "tb-a,tb-b" {
		t.Fatalf("first request tried %s", got)
	}
	if env, resp := execute(t, h, chatBody(10, "")); !env.OK || string(resp.Payload) != `{"from":"tb-b"}` {
		t.Fatalf("second request: %+v %s", env, resp.Payload)
	}
	if got := strings.Join(fh.models(), ","); got != "tb-a,tb-b,tb-b" {
		t.Errorf("host saw %s, want the second request to skip the cooling tb-a", got)
	}
	if d := lastDecision(t, h); strings.Join(d.Skips, ",") != "tb-a:cooldown" {
		t.Errorf("second decision skips = %v", d.Skips)
	}
	h.now = func() time.Time { return clock.Add(61 * time.Second) } // past the 60s default cooldown (the longer of it and Retry-After)
	execute(t, h, chatBody(10, ""))
	if got := fh.models(); len(got) < 4 || got[3] != "tb-a" {
		t.Errorf("after the cooldown the first candidate must be tried again first, host saw %v", got)
	}
}

func TestExecute400IsReturnedWithoutFallback(t *testing.T) {
	fh := &fakeHost{handle: byModel(map[string][]byte{"tb-a": hostOK(400, `{"error":"bad schema"}`, nil)})}
	h := newExecutor(t, wrap(abc), "", fh)
	env, _ := execute(t, h, chatBody(10, ""))
	if env.OK || env.Error.HTTPStatus != 400 || env.Error.Code != wire.ErrorCodeRequestScoped || !strings.Contains(env.Error.Message, "bad schema") {
		t.Fatalf("execute = %+v", env.Error)
	}
	if got := strings.Join(fh.models(), ","); got != "tb-a" {
		t.Errorf("host saw %s: a 400 blames the request and must not fall back", got)
	}
	execute(t, h, chatBody(10, ""))
	if got := strings.Join(fh.models(), ","); got != "tb-a,tb-a" {
		t.Errorf("a 400 must not cool the candidate, host saw %s", got)
	}
}

func TestExecuteAllFailReturnsTheLastErrorWithItsStatus(t *testing.T) {
	fh := &fakeHost{handle: byModel(map[string][]byte{
		"tb-a": hostOK(503, `down`, nil),
		"tb-b": hostFail(502, "bad gateway"),
		"tb-c": hostOK(429, `{"error":"quota"}`, map[string][]string{"Retry-After": {"7"}}),
	})}
	h := newExecutor(t, wrap(abc), "", fh)
	env, _ := execute(t, h, chatBody(10, ""))
	if env.OK || env.Error.HTTPStatus != 429 || env.Error.RetryAfterSeconds != 7 || !env.Error.Retryable ||
		!strings.Contains(env.Error.Message, "3 attempt(s) failed") || !strings.Contains(env.Error.Message, "tb-c") {
		t.Fatalf("execute = %+v", env.Error)
	}
	if d := lastDecision(t, h); len(d.Attempts) != 3 || d.Status != 429 {
		t.Errorf("decision = %+v", d)
	}

	// A host that answers nothing at all is a 502, still after trying every candidate.
	down := &fakeHost{handle: func(wire.HostModelRequest) []byte { return nil }}
	h2 := newExecutor(t, wrap(abc), "", down)
	if env, _ := execute(t, h2, chatBody(10, "")); env.OK || env.Error.HTTPStatus != 502 || len(down.models()) != 3 {
		t.Errorf("silent host: %+v after %d calls", env.Error, len(down.models()))
	}
}

func TestExecuteHonoursMaxAttempts(t *testing.T) {
	fh := &fakeHost{handle: func(wire.HostModelRequest) []byte { return hostOK(503, "down", nil) }}
	h := newExecutor(t, wrap(abc), "max-attempts: 2\n", fh)
	env, _ := execute(t, h, chatBody(10, ""))
	if env.OK || env.Error.HTTPStatus != 503 || !strings.Contains(env.Error.Message, "max-attempts 2 reached") {
		t.Fatalf("execute = %+v", env.Error)
	}
	if got := strings.Join(fh.models(), ","); got != "tb-a,tb-b" {
		t.Errorf("host saw %s, want exactly 2 attempts", got)
	}
}

func TestExecuteHonoursTheDeadline(t *testing.T) {
	release := make(chan struct{})
	t.Cleanup(func() { close(release) })
	fh := &fakeHost{handle: func(req wire.HostModelRequest) []byte {
		if req.Model == "tb-a" {
			<-release // the host call cannot be cancelled: the executor must stop waiting by itself
		}
		return hostOK(200, `{}`, nil)
	}}
	h := newExecutor(t, wrap(abc), "", fh)
	h.execTimeout = 50 * time.Millisecond
	began := time.Now()
	env, _ := execute(t, h, chatBody(10, ""))
	if env.OK || env.Error.HTTPStatus != 504 || env.Error.Code != wire.ErrorCodeDeadline {
		t.Fatalf("execute = %+v", env.Error)
	}
	if took := time.Since(began); took > 5*time.Second {
		t.Errorf("took %s: the deadline was not honoured", took)
	}
	if got := strings.Join(fh.models(), ","); got != "tb-a" {
		t.Errorf("host saw %s: no further attempt after the deadline", got)
	}
}

func TestExecuteNeverExecutesAnAlias(t *testing.T) {
	// Another alias' name used as a candidate model would make the host route the call back to the
	// plugin: the executor skips it. (A candidate named like its own alias is rejected at load.)
	router := `{"schema":"tierbridge-router/2","target":"host","aliases":[
	 {"alias":"tierbridge/auto","candidates":[{"model":"tierbridge/other"},{"model":"TierBridge/Auto2"},{"model":"tb-a"}]},
	 {"alias":"tierbridge/other","candidates":[]},{"alias":"tierbridge/auto2","candidates":[]}]}`
	fh := &fakeHost{handle: byModel(nil)}
	h := newExecutor(t, router, "", fh)
	env, _ := execute(t, h, chatBody(10, ""))
	if !env.OK || strings.Join(fh.models(), ",") != "tb-a" {
		t.Fatalf("host saw %v (%+v), want only tb-a", fh.models(), env.Error)
	}
	if d := lastDecision(t, h); strings.Join(d.Skips, ",") != "tierbridge/other:alias,TierBridge/Auto2:alias" {
		t.Errorf("skips = %v", d.Skips)
	}
}

func TestExecutorPanicsBecomeCleanErrors(t *testing.T) {
	// A panic inside the executor: an error envelope with a status, and the plugin keeps serving.
	fh := &fakeHost{handle: byModel(nil)}
	h := newExecutor(t, wrap(abc), "debug-force-error: true\n", fh)
	env, _ := execute(t, h, chatBody(10, ""))
	if env.OK || env.Error.Code != wire.ErrorCodeInternal || env.Error.HTTPStatus != 500 || strings.Contains(env.Error.Message, "debug-force-error") {
		t.Fatalf("panic envelope = %+v", env.Error)
	}
	if len(fh.models()) != 0 {
		t.Errorf("the host was called after the panic: %v", fh.models())
	}
	if env := decode(t, h.Handle(wire.MethodPluginReconfigure, lifecycle(t, h.routerFileYAML())), nil); !env.OK {
		t.Fatal("reconfigure failed")
	}
	if env, _ := execute(t, h, chatBody(10, "")); !env.OK {
		t.Errorf("the plugin did not recover after the panic: %+v", env.Error)
	}

	// A panic inside the host callback is one failed attempt: the next candidate answers.
	calls := 0
	boom := &fakeHost{handle: func(req wire.HostModelRequest) []byte {
		if calls++; calls == 1 {
			panic("callback exploded")
		}
		return hostOK(200, `{"from":"`+req.Model+`"}`, nil)
	}}
	h2 := newExecutor(t, wrap(abc), "", boom)
	env2, resp := execute(t, h2, chatBody(10, ""))
	if !env2.OK || string(resp.Payload) != `{"from":"tb-b"}` {
		t.Fatalf("after a callback panic: %+v / %s", env2.Error, resp.Payload)
	}
	if d := lastDecision(t, h2); !strings.Contains(strings.Join(d.Attempts, "|"), "tb-a:0:ok=false:fallback=true") {
		t.Errorf("attempts = %v", d.Attempts)
	}
}

func TestExecutorSkipsAndNoCandidate(t *testing.T) {
	// rpm 1: the second request finds the only candidate used up and answers 503 without calling the host.
	router := wrap(`{"model":"tb-a","cost":{"status":"free"},"quota":{"rpm":1}}`)
	fh := &fakeHost{handle: byModel(nil)}
	h := newExecutor(t, router, "", fh)
	if env, _ := execute(t, h, chatBody(10, "")); !env.OK {
		t.Fatalf("first: %+v", env.Error)
	}
	env, _ := execute(t, h, chatBody(10, ""))
	if env.OK || env.Error.HTTPStatus != 503 || env.Error.Code != wire.ErrorCodeNoRoute || !strings.Contains(env.Error.Message, "tb-a rpm") {
		t.Errorf("second: %+v", env.Error)
	}
	if len(fh.models()) != 1 {
		t.Errorf("host calls = %v", fh.models())
	}
	// A daily budget that no priced candidate fits.
	h2 := newExecutor(t, wrap(`{"model":"tb-p","cost":{"status":"known","input":100,"output":100}}`), "daily-budget-usd: 0.0001\n", &fakeHost{handle: byModel(nil)})
	if env, _ := execute(t, h2, chatBody(4000, "")); env.OK || !strings.Contains(env.Error.Message, "budget") {
		t.Errorf("budget: %+v", env.Error)
	}
}

func TestExecutorOtherMethods(t *testing.T) {
	h, _ := newPlugin(t, wrap(abc), "")
	var id wire.IdentifierResponse
	if env := decode(t, h.Handle(wire.MethodExecutorIdentifier, nil), &id); !env.OK || id.Identifier != PluginName {
		t.Errorf("identifier = %+v %+v", env, id)
	}
	for _, m := range []string{wire.MethodExecutorCountTokens, wire.MethodExecutorHTTP} {
		env := decode(t, h.Handle(m, execRequest(t, chatBody(5, ""))), nil)
		if env.OK || env.Error.Code != wire.ErrorCodeNotImplemented || env.Error.HTTPStatus != 501 {
			t.Errorf("%s = %+v, want not_implemented", m, env.Error)
		}
	}
	stream, _ := wire.Marshal(wire.ExecutorRequest{Model: "tierbridge/auto", Stream: true, Payload: []byte(chatBody(5, ""))})
	if env := decode(t, h.Handle(wire.MethodExecutorExecute, stream), nil); env.OK || env.Error.Code != wire.ErrorCodeNotImplemented {
		t.Errorf("stream execute = %+v", env.Error)
	}
	if env := decode(t, h.Handle(wire.MethodExecutorExecute, []byte(`{nope`)), nil); env.OK || env.Error.HTTPStatus != 400 {
		t.Errorf("bad request = %+v", env.Error)
	}
	if env, _ := execute(t, h, chatBody(5, "")); env.OK || env.Error.HTTPStatus != 503 {
		t.Errorf("without a host callback: %+v", env.Error)
	}
}

func TestDecisionsEndpointListsAttempts(t *testing.T) {
	fh := &fakeHost{handle: byModel(map[string][]byte{"tb-a": hostOK(503, "x", nil)})}
	h := newExecutor(t, wrap(abc), "", fh)
	execute(t, h, chatBody(10, ""))
	var doc struct {
		Decisions []struct {
			Chosen *struct {
				Model  string
				Status int
			}
			Attempts []struct {
				Model    string
				Status   int
				OK       bool
				Fallback bool
			}
		}
	}
	resp, _ := manage(t, h, "/tierbridge/decisions", nil)
	if resp.StatusCode != 200 {
		t.Fatalf("status %d: %s", resp.StatusCode, resp.Body)
	}
	if err := json.Unmarshal(resp.Body, &doc); err != nil || len(doc.Decisions) != 1 {
		t.Fatalf("decisions = %s (%v)", resp.Body, err)
	}
	d := doc.Decisions[0]
	if d.Chosen == nil || d.Chosen.Model != "tb-b" || len(d.Attempts) != 2 || d.Attempts[0].Status != 503 || !d.Attempts[0].Fallback || !d.Attempts[1].OK {
		t.Errorf("decision = %+v", d)
	}
}

func TestGoldenRegisterCapabilities(t *testing.T) {
	raw, err := wire.Marshal(Registration())
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Capabilities json.RawMessage `json:"capabilities"`
		Metadata     struct{ ConfigFields []json.RawMessage }
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	const wantCaps = `{"model_router":true,"executor":true,"executor_model_scope":"static",` +
		`"executor_input_formats":["openai","openai-response","claude","gemini"],` +
		`"executor_output_formats":["openai","openai-response","claude","gemini"],` +
		`"usage_plugin":true,"management_api":true}`
	if string(doc.Capabilities) != wantCaps {
		t.Errorf("capabilities\n got %s\nwant %s", doc.Capabilities, wantCaps)
	}
	var have []string
	for _, f := range doc.Metadata.ConfigFields {
		have = append(have, string(f))
	}
	joined := strings.Join(have, "\n")
	for _, want := range []string{
		`{"Name":"max-attempts","Type":"integer","Default":3,"EnumValues":null,"Description":"How many candidates one request may try through the host before the last error is returned (1 to 50)."}`,
		`{"Name":"deadline-seconds","Type":"integer","Default":120,"EnumValues":null,"Description":"Seconds one request may take across all its attempts before it is answered with 504. 0 means no deadline."}`,
	} {
		if !strings.Contains(joined, want) {
			t.Errorf("config fields lack %s", want)
		}
	}
	if strings.Contains(string(raw), `"provider"`) {
		t.Errorf("registration still mentions a provider: %s", raw)
	}
}

func TestGoldenExecutorWire(t *testing.T) {
	fh := &fakeHost{handle: byModel(map[string][]byte{"tb-a": hostOK(200, `{"ok":1}`, map[string][]string{"Content-Type": {"application/json"}, "Content-Length": {"8"}})})}
	h := newExecutor(t, wrap(abc), "", fh)
	body := `{"model":"tierbridge/auto","messages":[]}`
	rawResp := h.Handle(wire.MethodExecutorExecute, execRequest(t, body))
	wantResp := `{"ok":true,"result":{"Payload":"` + base64.StdEncoding.EncodeToString([]byte(`{"ok":1}`)) + `","Headers":{"Content-Type":["application/json"]}}}`
	if string(rawResp) != wantResp {
		t.Errorf("executor.execute response\n got %s\nwant %s", rawResp, wantResp)
	}
	wantReq := `{"entry_protocol":"openai","exit_protocol":"openai","model":"tb-a","stream":false,"body":"` +
		base64.StdEncoding.EncodeToString([]byte(`{"messages":[],"model":"tb-a"}`)) +
		`","headers":{"Authorization":["Bearer k"],"Content-Type":["application/json"]},"query":{},"alt":"","host_callback_id":"cb-1"}`
	if got := string(fh.raw[0]); got != wantReq {
		t.Errorf("host.model.execute request\n got %s\nwant %s", got, wantReq)
	}
	// The request the host sends carries Go field names (no json tags) except the two snake_case ids.
	if got := string(execRequest(t, `{}`)); !strings.HasPrefix(got, `{"Model":"tierbridge/auto","Format":"openai","Stream":false,"Alt":"","Headers":`) || !strings.HasSuffix(got, `"host_callback_id":"cb-1"}`) {
		t.Errorf("executor request wire = %s", got)
	}
	if got := string(h.Handle(wire.MethodExecutorCountTokens, nil)); got != `{"ok":false,"error":{"code":"not_implemented","message":"token counting is not supported by the tierbridge executor","http_status":501}}` {
		t.Errorf("count_tokens = %s", got)
	}
	if got := string(h.Handle(wire.MethodExecutorIdentifier, nil)); got != `{"ok":true,"result":{"identifier":"tierbridge"}}` {
		t.Errorf("identifier = %s", got)
	}
	routeRaw := h.Handle(wire.MethodModelRoute, hostRouteRequest(t, "tierbridge/auto", chatBody(10, ""), nil))
	if got := string(routeRaw); got != `{"ok":true,"result":{"Handled":true,"TargetKind":"self","Target":"","TargetModel":"","Reason":"rank 1 of 3"}}` {
		t.Errorf("model.route = %s", got)
	}
	failRaw := wire.ErrorEnvelopeStatus(wire.ErrorCodeUpstream, "boom", 503, 12)
	if got := string(failRaw); got != `{"ok":false,"error":{"code":"upstream_error","message":"boom","http_status":503,"retryable":true,"retry_after_seconds":12}}` {
		t.Errorf("error envelope = %s", got)
	}
}

func TestExecuteConcurrently(t *testing.T) {
	var n int
	var mu sync.Mutex
	fh := &fakeHost{handle: func(req wire.HostModelRequest) []byte {
		mu.Lock()
		n++
		fail := n%3 == 0
		mu.Unlock()
		if fail && req.Model == "tb-a" {
			return hostOK(503, "x", nil)
		}
		return hostOK(200, `{}`, nil)
	}}
	h := newExecutor(t, wrap(abc), "decisions-keep: 5\n", fh)
	var wg sync.WaitGroup
	for g := range 12 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for range 15 {
				switch g % 4 {
				case 0:
					h.Handle(wire.MethodModelRoute, hostRouteRequest(t, "tierbridge/auto", chatBody(10, ""), nil))
				case 1:
					h.Status()
					h.Handle(wire.MethodManagementHandle, hostManagementRequest(t, "GET", "/tierbridge/decisions", nil))
				case 2:
					h.Handle(wire.MethodUsageHandle, []byte(`{"Model":"tb-b","Failed":true,"Failure":{"StatusCode":429}}`))
				default:
					h.Handle(wire.MethodExecutorExecute, execRequest(t, chatBody(10, "")))
				}
			}
		}()
	}
	wg.Wait()
	if len(fh.models()) == 0 {
		t.Error("no host call was made")
	}
}
