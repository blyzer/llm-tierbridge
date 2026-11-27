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

// scriptChunk is one stream_read answer; err ends the stream with that error.
type scriptChunk struct{ payload, err string }

// streamScript is how one candidate model behaves: startStatus >= 400 fails host.model.execute_stream,
// otherwise the chunks are served by host.model.stream_read in order and the stream ends after them.
type streamScript struct {
	startStatus int
	chunks      []scriptChunk
}

// fakeStreamHost plays the host side of streaming: the inner model streams and the outer stream the
// plugin emits into.
type fakeStreamHost struct {
	mu           sync.Mutex
	scripts      map[string]streamScript
	inner        map[string][]scriptChunk // inner stream id -> chunks left
	n            int
	started      []string
	closedInner  []string
	emitted      []string
	outerClosed  []wire.StreamCloseRequest
	raw          map[string][]byte // last raw request per method
	emitFailFrom int               // the (emitFailFrom+1)th emit and later fail like a closed stream; -1 never
}

func newStreamHost(scripts map[string]streamScript) *fakeStreamHost {
	return &fakeStreamHost{scripts: scripts, inner: map[string][]scriptChunk{}, raw: map[string][]byte{}, emitFailFrom: -1}
}

func (f *fakeStreamHost) Call(method string, request []byte) ([]byte, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.raw[method] = append([]byte(nil), request...)
	switch method {
	case wire.MethodHostModelExecuteStream:
		var req wire.HostModelRequest
		if err := json.Unmarshal(request, &req); err != nil || !req.Stream {
			return nil, fmt.Errorf("bad stream start %s %v", request, err)
		}
		f.started = append(f.started, req.Model)
		sc := f.scripts[req.Model]
		if sc.startStatus >= 400 {
			return hostFail(sc.startStatus, "start failed"), nil
		}
		f.n++
		id := fmt.Sprintf("in-%d", f.n)
		f.inner[id] = append([]scriptChunk(nil), sc.chunks...)
		raw, _ := wire.OKEnvelope(wire.HostModelStreamResponse{StatusCode: 200, StreamID: id, Headers: map[string][]string{"Content-Type": {"text/event-stream"}, "Content-Length": {"1"}}})
		return raw, nil
	case wire.MethodHostModelStreamRead:
		var ref wire.HostModelStreamRef
		_ = json.Unmarshal(request, &ref)
		left, ok := f.inner[ref.StreamID]
		if !ok {
			return hostFail(404, "unknown stream"), nil
		}
		chunk := wire.HostModelStreamChunk{Done: true}
		if len(left) > 0 {
			c := left[0]
			f.inner[ref.StreamID] = left[1:]
			chunk = wire.HostModelStreamChunk{Payload: []byte(c.payload), Error: c.err, Done: c.err != "" || len(left) == 1}
		}
		raw, _ := wire.OKEnvelope(chunk)
		return raw, nil
	case wire.MethodHostModelStreamClose:
		var ref wire.HostModelStreamRef
		_ = json.Unmarshal(request, &ref)
		f.closedInner = append(f.closedInner, ref.StreamID)
		raw, _ := wire.OKEnvelope(struct{}{})
		return raw, nil
	case wire.MethodHostStreamEmit:
		var e wire.StreamEmitRequest
		_ = json.Unmarshal(request, &e)
		f.emitted = append(f.emitted, string(e.Payload))
		if f.emitFailFrom >= 0 && len(f.emitted) > f.emitFailFrom {
			return hostFail(409, "stream closed"), nil
		}
		raw, _ := wire.OKEnvelope(struct{}{})
		return raw, nil
	case wire.MethodHostStreamClose:
		var c wire.StreamCloseRequest
		_ = json.Unmarshal(request, &c)
		f.outerClosed = append(f.outerClosed, c)
		raw, _ := wire.OKEnvelope(struct{}{})
		return raw, nil
	}
	return nil, fmt.Errorf("unexpected host method %q", method)
}

func (f *fakeStreamHost) snap() (started, closedInner, emitted []string, outer []wire.StreamCloseRequest) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.started...), append([]string(nil), f.closedInner...), append([]string(nil), f.emitted...), append([]wire.StreamCloseRequest(nil), f.outerClosed...)
}

// settled waits until the background pump is done: the outer stream was closed, or (client gone) every
// inner stream that was opened has been closed.
func (f *fakeStreamHost) settled(t *testing.T, outer, inner int) {
	t.Helper()
	for end := time.Now().Add(5 * time.Second); time.Now().Before(end); time.Sleep(2 * time.Millisecond) {
		_, ci, _, oc := f.snap()
		if len(oc) >= outer && len(ci) >= inner {
			return
		}
	}
	_, ci, em, oc := f.snap()
	t.Fatalf("stream did not settle: outer closes %v, inner closes %v, emitted %v", oc, ci, em)
}

func streamRequest(t *testing.T, body, streamID string) []byte {
	t.Helper()
	raw, err := wire.Marshal(wire.ExecutorRequest{
		Model: "tierbridge/auto", Format: "openai", SourceFormat: "openai", Stream: true,
		Headers: map[string][]string{"Content-Type": {"application/json"}}, Query: map[string][]string{},
		OriginalRequest: []byte(body), Payload: []byte(body), StreamID: streamID, HostCallbackID: "cb-1",
	})
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func newStreamer(t *testing.T, sh *fakeStreamHost, yaml string) *Handler {
	t.Helper()
	h, _ := newPlugin(t, wrap(abc), yaml)
	h.SetHost(sh)
	return h
}

func startStream(t *testing.T, h *Handler, id string) wire.Envelope {
	t.Helper()
	return decode(t, h.Handle(wire.MethodExecutorStream, streamRequest(t, chatBody(10, ""), id)), nil)
}

func TestStreamForwardsEveryChunkAndClosesBothStreams(t *testing.T) {
	sh := newStreamHost(map[string]streamScript{"tb-a": {chunks: []scriptChunk{{payload: "data: 1\n\n"}, {payload: "data: 2\n\n"}, {payload: "data: [DONE]\n\n"}}}})
	h := newStreamer(t, sh, "")
	var resp wire.ExecutorStreamResponse
	env := decode(t, h.Handle(wire.MethodExecutorStream, streamRequest(t, chatBody(10, ""), "out-1")), &resp)
	if !env.OK || resp.Headers["Content-Type"][0] != "text/event-stream" || resp.Headers["Content-Length"] != nil {
		t.Fatalf("execute_stream = %+v / %+v", env, resp)
	}
	sh.settled(t, 1, 1)
	started, closed, emitted, outer := sh.snap()
	if strings.Join(started, ",") != "tb-a" || strings.Join(emitted, "|") != "data: 1\n\n|data: 2\n\n|data: [DONE]\n\n" {
		t.Errorf("started %v emitted %q", started, emitted)
	}
	if len(outer) != 1 || outer[0].StreamID != "out-1" || outer[0].ErrorDetail != nil || strings.Join(closed, ",") != "in-1" {
		t.Errorf("outer closes %+v, inner closes %v", outer, closed)
	}
	if d := lastDecision(t, h); len(d.Attempts) != 1 || d.Attempts[0] != "tb-a:200:ok=true:fallback=false:cool=false" {
		t.Errorf("attempts = %v", d.Attempts)
	}
}

func TestStreamStartFailureFallsBackToTheNextCandidate(t *testing.T) {
	sh := newStreamHost(map[string]streamScript{"tb-a": {startStatus: 503}, "tb-b": {chunks: []scriptChunk{{payload: "b1"}, {payload: "b2"}}}})
	h := newStreamer(t, sh, "")
	if env := startStream(t, h, "out-1"); !env.OK {
		t.Fatalf("execute_stream: %+v", env.Error)
	}
	sh.settled(t, 1, 1)
	started, _, emitted, _ := sh.snap()
	if strings.Join(started, ",") != "tb-a,tb-b" || strings.Join(emitted, ",") != "b1,b2" {
		t.Errorf("started %v emitted %v", started, emitted)
	}
	want := []string{"tb-a:503:ok=false:fallback=true:cool=true", "tb-b:200:ok=true:fallback=false:cool=false"}
	if d := lastDecision(t, h); strings.Join(d.Attempts, "|") != strings.Join(want, "|") {
		t.Errorf("attempts = %v, want %v", d.Attempts, want)
	}
	// The failed start cooled tb-a: the next stream does not try it.
	if env := startStream(t, h, "out-2"); !env.OK {
		t.Fatal("second stream failed")
	}
	sh.settled(t, 2, 2)
	if started, _, _, _ := sh.snap(); strings.Join(started, ",") != "tb-a,tb-b,tb-b" {
		t.Errorf("second stream started %v", started)
	}
}

func TestStreamErrorBeforeTheFirstChunkFallsBack(t *testing.T) {
	sh := newStreamHost(map[string]streamScript{"tb-a": {chunks: []scriptChunk{{err: "connection reset"}}}, "tb-b": {chunks: []scriptChunk{{payload: "ok"}}}})
	h := newStreamer(t, sh, "")
	if env := startStream(t, h, "out-1"); !env.OK {
		t.Fatalf("execute_stream: %+v", env.Error)
	}
	sh.settled(t, 1, 2)
	started, closed, emitted, outer := sh.snap()
	if strings.Join(started, ",") != "tb-a,tb-b" || strings.Join(emitted, ",") != "ok" || outer[0].ErrorDetail != nil {
		t.Errorf("started %v emitted %v outer %+v", started, emitted, outer)
	}
	if strings.Join(closed, ",") != "in-1,in-2" {
		t.Errorf("inner closes = %v, want the failed stream closed before the fallback", closed)
	}
	if d := lastDecision(t, h); len(d.Attempts) != 2 || d.Attempts[0] != "tb-a:502:ok=false:fallback=true:cool=true" {
		t.Errorf("attempts = %v", d.Attempts)
	}
}

func TestStreamErrorAfterTheFirstChunkIsNotRetried(t *testing.T) {
	sh := newStreamHost(map[string]streamScript{"tb-a": {chunks: []scriptChunk{{payload: "first"}, {err: "upstream died"}}}})
	h := newStreamer(t, sh, "")
	if env := startStream(t, h, "out-1"); !env.OK {
		t.Fatalf("execute_stream: %+v", env.Error)
	}
	sh.settled(t, 1, 1)
	started, closed, emitted, outer := sh.snap()
	if strings.Join(started, ",") != "tb-a" || strings.Join(emitted, ",") != "first" {
		t.Errorf("started %v emitted %v: no fallback once a chunk was forwarded", started, emitted)
	}
	if len(outer) != 1 || outer[0].ErrorDetail == nil || outer[0].ErrorDetail.HTTPStatus != 502 || !strings.Contains(outer[0].ErrorDetail.Message, "upstream died") || strings.Join(closed, ",") != "in-1" {
		t.Errorf("outer %+v inner closes %v", outer, closed)
	}
}

func TestStreamClientDisconnectClosesTheInnerStream(t *testing.T) {
	sh := newStreamHost(map[string]streamScript{"tb-a": {chunks: []scriptChunk{{payload: "a"}, {payload: "b"}, {payload: "c"}}}})
	sh.emitFailFrom = 1 // the host closed the outer stream after the first chunk
	h := newStreamer(t, sh, "")
	if env := startStream(t, h, "out-1"); !env.OK {
		t.Fatalf("execute_stream: %+v", env.Error)
	}
	sh.settled(t, 0, 1)
	_, closed, emitted, outer := sh.snap()
	if strings.Join(closed, ",") != "in-1" || len(emitted) != 2 || len(outer) != 0 {
		t.Errorf("inner closes %v, emits %v, outer closes %+v: the pump must stop at the failed emit", closed, emitted, outer)
	}
}

func TestStreamAllCandidatesFailBeforeOutput(t *testing.T) {
	sh := newStreamHost(map[string]streamScript{"tb-a": {startStatus: 503}, "tb-b": {startStatus: 429}, "tb-c": {chunks: []scriptChunk{{err: "reset"}}}})
	h := newStreamer(t, sh, "")
	env := startStream(t, h, "out-1")
	if env.OK || env.Error.HTTPStatus != 502 || !strings.Contains(env.Error.Message, "3 attempt(s) failed") {
		t.Fatalf("execute_stream = %+v", env.Error)
	}
	if _, closed, emitted, outer := sh.snap(); len(emitted) != 0 || len(outer) != 0 || len(closed) != 1 {
		t.Errorf("nothing may reach the outer stream: emitted %v outer %v closed %v", emitted, outer, closed)
	}
	// A stream request without the host's stream id is refused, not guessed.
	if env := decode(t, h.Handle(wire.MethodExecutorStream, streamRequest(t, chatBody(5, ""), "")), nil); env.OK || env.Error.HTTPStatus != 400 {
		t.Errorf("missing stream_id = %+v", env.Error)
	}
}

func TestRouteTakesStreamingRequestsToo(t *testing.T) {
	h, _ := newPlugin(t, wrap(abc), "")
	raw, _ := wire.Marshal(struct {
		RequestedModel string
		Stream         bool
		Body           []byte
	}{"tierbridge/auto", true, []byte(chatBody(10, ""))})
	var got wire.ModelRouteResponse
	decode(t, h.Handle(wire.MethodModelRoute, raw), &got)
	if !got.Handled || got.TargetKind != wire.ModelRouteTargetSelf {
		t.Errorf("streaming route = %+v, want Handled with TargetKind self", got)
	}
}

func TestGoldenStreamWire(t *testing.T) {
	sh := newStreamHost(map[string]streamScript{"tb-a": {chunks: []scriptChunk{{payload: "x"}}}})
	h := newStreamer(t, sh, "")
	got := string(h.Handle(wire.MethodExecutorStream, streamRequest(t, `{"model":"tierbridge/auto","stream":true}`, "out-1")))
	if want := `{"ok":true,"result":{"headers":{"Content-Type":["text/event-stream"]}}}`; got != want {
		t.Errorf("execute_stream response\n got %s\nwant %s", got, want)
	}
	sh.settled(t, 1, 1)
	sh.mu.Lock()
	defer sh.mu.Unlock()
	wantStart := `{"entry_protocol":"openai","exit_protocol":"openai","model":"tb-a","stream":true,"body":"` +
		base64.StdEncoding.EncodeToString([]byte(`{"model":"tb-a","stream":true}`)) +
		`","headers":{"Content-Type":["application/json"]},"query":{},"alt":"","host_callback_id":"cb-1"}`
	for method, want := range map[string]string{
		wire.MethodHostModelExecuteStream: wantStart,
		wire.MethodHostModelStreamRead:    `{"stream_id":"in-1"}`,
		wire.MethodHostModelStreamClose:   `{"stream_id":"in-1"}`,
		wire.MethodHostStreamEmit:         `{"stream_id":"out-1","payload":"eA=="}`,
		wire.MethodHostStreamClose:        `{"stream_id":"out-1"}`,
	} {
		if got := string(sh.raw[method]); got != want {
			t.Errorf("%s request\n got %s\nwant %s", method, got, want)
		}
	}
	detail := wire.StreamCloseRequest{StreamID: "out-1", Error: "boom", ErrorDetail: &wire.Error{Code: "upstream_error", Message: "boom", HTTPStatus: 502}}
	if raw, _ := wire.Marshal(detail); string(raw) != `{"stream_id":"out-1","error":"boom","error_detail":{"code":"upstream_error","message":"boom","http_status":502}}` {
		t.Errorf("close with error = %s", raw)
	}
}

func TestStreamsConcurrently(t *testing.T) {
	scripts := map[string]streamScript{
		"tb-a": {chunks: []scriptChunk{{payload: "a1"}, {payload: "a2"}}},
		"tb-b": {chunks: []scriptChunk{{payload: "b1"}, {err: "late failure"}}},
	}
	sh := newStreamHost(scripts)
	h := newStreamer(t, sh, "decisions-keep: 5\n")
	const streams = 16
	var wg sync.WaitGroup
	for i := range streams {
		wg.Add(1)
		go func() {
			defer wg.Done()
			startStream(t, h, fmt.Sprintf("out-%d", i))
			h.Status()
			h.Handle(wire.MethodUsageHandle, []byte(`{"Model":"tb-a","Failed":true,"Failure":{"StatusCode":503}}`))
		}()
	}
	wg.Wait()
	sh.settled(t, streams, streams)
}
