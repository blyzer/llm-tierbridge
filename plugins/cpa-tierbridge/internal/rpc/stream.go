package rpc

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

// hostCall runs one host callback to completion and decodes its envelope: the result on success, else
// the status and message the host reported.
func hostCall(host HostCaller, method string, req any, wait time.Duration, bounded bool) (result json.RawMessage, status int, errMsg string, timedOut bool) {
	payload, err := wire.Marshal(req)
	if err != nil {
		return nil, 0, "encode " + method + ": " + err.Error(), false
	}
	env, err, timedOut := callHost(host, method, payload, wait, bounded)
	switch {
	case timedOut:
		return nil, 0, "deadline exceeded", true
	case err != nil:
		return nil, 0, err.Error(), false
	}
	var e wire.Envelope
	if err := json.Unmarshal(env, &e); err != nil {
		return nil, 0, "undecodable host envelope: " + err.Error(), false
	}
	if !e.OK {
		if e.Error == nil {
			return nil, 0, method + " failed", false
		}
		return nil, e.Error.HTTPStatus, strings.TrimSpace(e.Error.Code + ": " + e.Error.Message), false
	}
	return e.Result, 0, "", false
}

// closeInner releases an inner stream of the host; failures are irrelevant (the host also closes it
// with the callback scope), so it never blocks on the answer longer than the call itself.
func closeInner(host HostCaller, id string) {
	if id != "" {
		hostCall(host, wire.MethodHostModelStreamClose, wire.HostModelStreamRef{StreamID: id}, 0, false)
	}
}

// attemptStream opens the inner stream of one candidate and waits for its first output chunk, within
// the request deadline. The attempt only counts as started once that chunk (or a clean end) arrived: a
// start status >= 400, a transport failure or a stream error before any output is a failed attempt the
// caller may follow with the next candidate. On every failure the inner stream is closed here.
func (h *Handler) attemptStream(host HostCaller, payload []byte, wait time.Duration, bounded bool) attemptResult {
	began := time.Now()
	left := func() time.Duration { return max(wait-time.Since(began), time.Millisecond) }
	env, err, timedOut := callHost(host, wire.MethodHostModelExecuteStream, payload, wait, bounded)
	if timedOut {
		return attemptResult{err: "deadline exceeded", timedOut: true}
	}
	if err != nil {
		return attemptResult{err: err.Error()}
	}
	var e wire.Envelope
	if err := json.Unmarshal(env, &e); err != nil {
		return attemptResult{err: "undecodable host envelope: " + err.Error()}
	}
	if !e.OK {
		if e.Error == nil {
			return attemptResult{err: "host callback failed"}
		}
		return attemptResult{status: e.Error.HTTPStatus, err: strings.TrimSpace(e.Error.Code + ": " + e.Error.Message)}
	}
	var start wire.HostModelStreamResponse
	if err := json.Unmarshal(e.Result, &start); err != nil {
		return attemptResult{err: "undecodable host.model.execute_stream result: " + err.Error()}
	}
	if start.StatusCode < 200 || start.StatusCode > 299 {
		closeInner(host, start.StreamID)
		return attemptResult{status: start.StatusCode, headers: start.Headers, err: fmt.Sprintf("status %d", start.StatusCode)}
	}
	if start.StreamID == "" {
		return attemptResult{status: http.StatusBadGateway, err: "host.model.execute_stream returned no stream_id"}
	}
	for {
		raw, _, msg, timedOut := hostCall(host, wire.MethodHostModelStreamRead, wire.HostModelStreamRef{StreamID: start.StreamID}, left(), bounded)
		if timedOut || msg != "" {
			closeInner(host, start.StreamID)
			if timedOut {
				return attemptResult{err: "deadline exceeded", timedOut: true}
			}
			return attemptResult{status: http.StatusBadGateway, err: "stream read: " + msg}
		}
		var chunk wire.HostModelStreamChunk
		if err := json.Unmarshal(raw, &chunk); err != nil {
			closeInner(host, start.StreamID)
			return attemptResult{status: http.StatusBadGateway, err: "undecodable stream chunk: " + err.Error()}
		}
		switch {
		case chunk.Error != "" && len(chunk.Payload) == 0:
			// The stream failed before any output: nothing reached the client, so another candidate may answer.
			closeInner(host, start.StreamID)
			return attemptResult{status: http.StatusBadGateway, err: "stream error before the first chunk: " + chunk.Error}
		case len(chunk.Payload) > 0 || chunk.Done:
			return attemptResult{status: start.StatusCode, headers: start.Headers, streamID: start.StreamID, first: chunk.Payload, done: chunk.Done && chunk.Error == ""}
		}
		// An empty keep-alive chunk is not output yet: keep waiting.
	}
}

// startPump answers executor.execute_stream with the winning attempt's headers and forwards the rest of
// the stream in the background (host.stream.emit per chunk, host.stream.close at the end). From the
// first forwarded chunk on there is no fallback: an error closes the stream with the error detail.
func (h *Handler) startPump(host HostCaller, outer string, res attemptResult) []byte {
	raw, err := wire.OKEnvelope(wire.ExecutorStreamResponse{Headers: filterHeaders(res.headers)})
	if err != nil {
		closeInner(host, res.streamID)
		return wire.ErrorEnvelope(wire.ErrorCodeInternal, "encode executor stream response")
	}
	go h.pump(host, outer, res)
	return raw
}

func (h *Handler) pump(host HostCaller, outer string, res attemptResult) {
	defer func() {
		if r := recover(); r != nil {
			h.noteError(fmt.Sprintf("stream pump: panic: %v", r))
			hostCall(host, wire.MethodHostStreamClose, wire.StreamCloseRequest{StreamID: outer, ErrorDetail: &wire.Error{
				Code: wire.ErrorCodeInternal, Message: "stream failed inside the plugin", HTTPStatus: http.StatusInternalServerError}}, 0, false)
		}
		closeInner(host, res.streamID) // always: end, error, client disconnect and panic alike
	}()
	// emit fails when the host closed the stream (client gone, context cancelled): stop and let the
	// deferred close release the inner stream.
	emit := func(p []byte) bool {
		_, _, msg, _ := hostCall(host, wire.MethodHostStreamEmit, wire.StreamEmitRequest{StreamID: outer, Payload: p}, 0, false)
		return msg == ""
	}
	finish := func(detail *wire.Error) {
		req := wire.StreamCloseRequest{StreamID: outer, ErrorDetail: detail}
		if detail != nil {
			req.Error = detail.Message
		}
		hostCall(host, wire.MethodHostStreamClose, req, 0, false)
	}
	if len(res.first) > 0 && !emit(res.first) {
		return
	}
	for done := res.done; !done; {
		raw, _, msg, _ := hostCall(host, wire.MethodHostModelStreamRead, wire.HostModelStreamRef{StreamID: res.streamID}, 0, false)
		if msg != "" {
			finish(&wire.Error{Code: wire.ErrorCodeUpstream, Message: "stream read failed: " + msg, HTTPStatus: http.StatusBadGateway})
			return
		}
		var chunk wire.HostModelStreamChunk
		if err := json.Unmarshal(raw, &chunk); err != nil {
			finish(&wire.Error{Code: wire.ErrorCodeUpstream, Message: "undecodable stream chunk: " + err.Error(), HTTPStatus: http.StatusBadGateway})
			return
		}
		if len(chunk.Payload) > 0 && !emit(chunk.Payload) {
			return
		}
		if chunk.Error != "" {
			finish(&wire.Error{Code: wire.ErrorCodeUpstream, Message: "upstream stream failed: " + chunk.Error, HTTPStatus: http.StatusBadGateway})
			return
		}
		done = chunk.Done
	}
	finish(nil)
}
