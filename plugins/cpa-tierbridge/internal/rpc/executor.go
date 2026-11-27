package rpc

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/accounting"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/estimate"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/selector"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

// ExecutorFormats are the client protocols the self executor declares as input and output. The host
// hands the request over in the client's own format (no translation) when it is listed, and the
// executor passes it through host.model.execute with that same format as entry and exit protocol.
var ExecutorFormats = []string{"openai", "openai-response", "claude", "gemini"}

// HostCaller is the plugin's way back into the host (the cgo layer implements it over the host API
// the plugin received at init; tests inject a fake). Call runs one host callback and returns the
// host's response envelope as bytes; the error is only for "no response at all" (no host API, no
// buffer): a failure the host reports is an envelope with ok:false. Call may block for as long as the
// host takes and cannot be cancelled: the executor stops waiting at its deadline, never the call.
type HostCaller interface {
	Call(method string, request []byte) ([]byte, error)
}

// SetHost installs the host callback. Without one, executor.execute answers 503.
func (h *Handler) SetHost(c HostCaller) {
	h.mu.Lock()
	h.host = c
	h.mu.Unlock()
}

func (h *Handler) hostCaller() HostCaller {
	h.mu.RLock()
	defer h.mu.RUnlock()
	return h.host
}

func notImplemented(msg string) []byte {
	return wire.ErrorEnvelopeStatus(wire.ErrorCodeNotImplemented, msg, http.StatusNotImplemented, 0)
}

// fallbackStatus reports whether a failed attempt should be followed by the next candidate. Status 0
// is "no answer" (transport failure). 429 and 5xx are the usual capacity/outage answers; 401/402/403/
// 404/408/425 mean this candidate's credential, quota or model name is the problem, not the request.
// Everything else below 500 (400, 413, 422, ...) blames the request itself: another model would
// answer the same, so it goes back to the client unchanged.
func fallbackStatus(status int) bool {
	switch status {
	case 0, 401, 402, 403, 404, 408, 425, 429:
		return true
	}
	return status >= 500
}

// attemptResult is what one host.model.execute call produced.
type attemptResult struct {
	status   int
	headers  map[string][]string
	body     []byte
	err      string // failure description; empty on success
	timedOut bool
}

// callModel runs one host.model.execute and stops waiting after wait (when bounded). A panic of the
// host callback is contained and reported as a failed attempt.
func callModel(host HostCaller, payload []byte, wait time.Duration, bounded bool) (env []byte, err error, timedOut bool) {
	type result struct {
		env []byte
		err error
	}
	ch := make(chan result, 1)
	go func() {
		defer func() {
			if r := recover(); r != nil {
				ch <- result{err: fmt.Errorf("host callback panic: %v", r)}
			}
		}()
		env, err := host.Call(wire.MethodHostModelExecute, payload)
		ch <- result{env: env, err: err}
	}()
	if !bounded {
		r := <-ch
		return r.env, r.err, false
	}
	t := time.NewTimer(wait)
	defer t.Stop()
	select {
	case r := <-ch:
		return r.env, r.err, false
	case <-t.C:
		return nil, errors.New("deadline exceeded"), true
	}
}

func (h *Handler) attempt(host HostCaller, payload []byte, wait time.Duration, bounded bool) attemptResult {
	env, err, timedOut := callModel(host, payload, wait, bounded)
	switch {
	case timedOut:
		return attemptResult{err: "deadline exceeded", timedOut: true}
	case err != nil:
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
	var resp wire.HostModelResponse
	if err := json.Unmarshal(e.Result, &resp); err != nil {
		return attemptResult{err: "undecodable host.model.execute result: " + err.Error()}
	}
	out := attemptResult{status: resp.StatusCode, headers: resp.Headers, body: resp.Body}
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		out.err = "status " + strconv.Itoa(resp.StatusCode) + excerpt(resp.Body)
	}
	return out
}

func excerpt(body []byte) string {
	body = bytes.TrimSpace(body)
	if len(body) == 0 {
		return ""
	}
	const max = 300
	if len(body) > max {
		return ": " + string(body[:max]) + "..."
	}
	return ": " + string(body)
}

// withModel sets "model" in a JSON object body to model, leaving every other key's bytes untouched.
// Bodies that are not an object or have no "model" (Gemini carries it in the path) pass unchanged.
func withModel(body []byte, model string) []byte {
	var m map[string]json.RawMessage
	if json.Unmarshal(body, &m) != nil {
		return body
	}
	if _, ok := m["model"]; !ok {
		return body
	}
	name, err := wire.Marshal(model)
	if err != nil {
		return body
	}
	m["model"] = name
	out, err := wire.Marshal(m)
	if err != nil {
		return body
	}
	return out
}

// dropHeaders are request/response headers that describe one HTTP hop and must not cross it.
var dropHeaders = map[string]bool{
	"content-length": true, "transfer-encoding": true, "connection": true, "host": true,
	"content-encoding": true, "accept-encoding": true, "keep-alive": true,
}

func filterHeaders(in map[string][]string) map[string][]string {
	if len(in) == 0 {
		return nil
	}
	out := make(map[string][]string, len(in))
	for k, v := range in {
		if !dropHeaders[strings.ToLower(k)] {
			out[k] = v
		}
	}
	return out
}

// execute serves executor.execute (non-streaming): run the alias' candidates through
// host.model.execute in rank order, falling back on failures that blame the candidate, within
// max-attempts and the deadline. The alias itself is never executed, so the host cannot route the
// call back into this plugin (it also skips this plugin's router for the callback).
func (h *Handler) execute(request []byte) []byte {
	cfg, store := h.config()
	if cfg.DebugForceError {
		panic("debug-force-error is set")
	}
	var req wire.ExecutorRequest
	if err := json.Unmarshal(request, &req); err != nil {
		return wire.ErrorEnvelopeStatus(wire.ErrorCodeBadRequest, "decode executor request: "+err.Error(), http.StatusBadRequest, 0)
	}
	if req.Stream {
		return notImplemented("streaming is not supported by the tierbridge executor yet")
	}
	body := req.Payload
	if len(bytes.TrimSpace(body)) == 0 {
		body = req.OriginalRequest
	}
	if len(bytes.TrimSpace(body)) == 0 {
		return wire.ErrorEnvelopeStatus(wire.ErrorCodeBadRequest, "executor request has no body", http.StatusBadRequest, 0)
	}
	est := estimate.FromBody(body)
	d := selector.Decision{Time: h.now(), RequestedModel: req.Model, EstTokens: est.Tokens, Tools: est.Tools, Image: est.Image, Handled: true}
	fail := func(status int, code, msg string, retryAfter int) []byte {
		d.Reason, d.Status = msg, status
		h.decisions.Add(d)
		return wire.ErrorEnvelopeStatus(code, msg, status, retryAfter)
	}

	host := h.hostCaller()
	if host == nil {
		return fail(http.StatusServiceUnavailable, wire.ErrorCodeNoRoute, "the host callback is not available", 0)
	}
	file, err := store.Current()
	entry := file.Alias(cfg.Alias)
	if entry == nil {
		msg := fmt.Sprintf("router file has no alias %q", cfg.Alias)
		if err != nil {
			msg = "router file: " + err.Error()
		}
		return fail(http.StatusServiceUnavailable, wire.ErrorCodeNoRoute, msg, 0)
	}
	aliases := map[string]bool{strings.ToLower(cfg.Alias): true}
	for _, a := range file.Aliases {
		aliases[strings.ToLower(a.Alias)] = true
	}
	d.Of = len(entry.Candidates)

	format := req.SourceFormat
	if format == "" {
		format = req.Format
	}
	if format == "" {
		format = "openai"
	}
	headers := filterHeaders(req.Headers)

	timeout := h.execTimeout
	if timeout == 0 {
		timeout = time.Duration(cfg.DeadlineSeconds) * time.Second
	}
	bounded := timeout > 0
	deadline := time.Now().Add(timeout)

	var (
		tried       int
		last        attemptResult
		lastModel   string
		deadlineHit bool
	)
	for i := range entry.Candidates {
		c := entry.Candidates[i]
		skip := func(why selector.SkipReason) {
			d.Skips = append(d.Skips, selector.Skip{Rank: i + 1, Model: c.Model, Reason: why})
		}
		if tried >= cfg.MaxAttempts {
			break
		}
		if aliases[strings.ToLower(c.Model)] {
			skip(selector.SkipAlias)
			continue
		}
		if why := selector.Fit(est, c); why != "" {
			skip(why)
			continue
		}
		if ok, why := h.acct.Reserve(c, est, h.now()); !ok {
			skip(why)
			continue
		}
		if bounded && !time.Now().Before(deadline) {
			deadlineHit = true
			break
		}
		payload, err := wire.Marshal(wire.HostModelRequest{
			EntryProtocol: format, ExitProtocol: format, Model: c.Model, Stream: false,
			Body: withModel(body, c.Model), Headers: headers, Query: req.Query, Alt: req.Alt,
			HostCallbackID: req.HostCallbackID,
		})
		if err != nil {
			return fail(http.StatusInternalServerError, wire.ErrorCodeInternal, "encode host request: "+err.Error(), 0)
		}
		tried++
		began := time.Now()
		res := h.attempt(host, payload, time.Until(deadline), bounded)
		at := selector.Attempt{Rank: i + 1, Model: c.Model, Status: res.status, Error: res.err, DurationMs: time.Since(began).Milliseconds()}
		last, lastModel = res, c.Model
		d.Model, d.Rank, d.Unpriced = c.Model, i+1, c.Unpriced()

		switch {
		case res.err == "":
			at.OK = true
			d.Attempts = append(d.Attempts, at)
			d.Reason, d.Status = selector.Outcome{Picked: &entry.Candidates[i], Rank: i + 1, Of: d.Of, Skips: d.Skips}.Reason(), res.status
			if tried > 1 {
				d.Reason += fmt.Sprintf(" after %d attempts", tried)
			}
			h.decisions.Add(d)
			raw, err := wire.OKEnvelope(wire.ExecutorResponse{Payload: res.body, Headers: filterHeaders(res.headers)})
			if err != nil {
				return wire.ErrorEnvelope(wire.ErrorCodeInternal, "encode executor response")
			}
			return raw
		case res.timedOut:
			d.Attempts = append(d.Attempts, at)
			deadlineHit = true
		default:
			at.CoolingDown = h.acct.Cool(c, res.status, res.headers, h.now())
			at.Fallback = fallbackStatus(res.status)
			d.Attempts = append(d.Attempts, at)
			if !at.Fallback {
				return fail(res.status, wire.ErrorCodeRequestScoped, fmt.Sprintf("%s answered %s", c.Model, res.err), 0)
			}
		}
		if deadlineHit {
			break
		}
	}

	switch {
	case deadlineHit:
		return fail(http.StatusGatewayTimeout, wire.ErrorCodeDeadline,
			fmt.Sprintf("deadline of %s exceeded after %d attempt(s); last: %s: %s", timeout, tried, lastModel, last.err), 0)
	case tried == 0:
		out := selector.Outcome{Of: d.Of, Skips: d.Skips}
		return fail(http.StatusServiceUnavailable, wire.ErrorCodeNoRoute, out.Reason(), 0)
	}
	status := last.status
	if status == 0 {
		status = http.StatusBadGateway
	}
	msg := fmt.Sprintf("%d attempt(s) failed; last: %s: %s", tried, lastModel, last.err)
	if tried >= cfg.MaxAttempts {
		msg += fmt.Sprintf(" (max-attempts %d reached)", cfg.MaxAttempts)
	}
	retry := int(accounting.RetryAfter(last.headers, h.now()).Seconds())
	return fail(status, wire.ErrorCodeUpstream, msg, retry)
}
