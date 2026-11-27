// Package rpc dispatches the host's plugin RPC calls (the JSON envelope protocol of CLIProxyAPI's
// native plugin ABI) to the tierbridge plugin logic. It has no cgo: the c-shared shim in package main
// only moves bytes in and out.
package rpc

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/accounting"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/config"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/estimate"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/selector"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

// Plugin identity published in plugin.register.
const (
	PluginName       = "tierbridge"
	PluginVersion    = "0.1.0"
	PluginAuthor     = "blyzer"
	PluginRepository = "https://github.com/blyzer/llm-tierbridge"
)

// Handler is the plugin's state and RPC entry point. The zero value is not usable: use NewHandler.
type Handler struct {
	now func() time.Time

	acct      *accounting.Accountant
	persist   *accounting.Persister
	decisions *selector.Recorder
	usageSeen *usageRing

	// beforeManagement is a test hook run before a management route is served.
	beforeManagement func(route string)

	mu        sync.RWMutex
	cfg       config.Config
	cfgErr    error
	store     *routerfile.Store
	lastErr   string
	lastErrAt time.Time
}

// NewHandler creates a handler with the default configuration (no router file: nothing is handled).
func NewHandler() *Handler {
	cfg := config.Default()
	h := &Handler{
		now:       time.Now,
		acct:      accounting.New(),
		decisions: selector.NewRecorder(cfg.DecisionsKeep),
		usageSeen: &usageRing{},
		cfg:       cfg,
		store:     routerfile.NewStore(cfg.RouterFile),
	}
	// The clock is read through h.now on every call, so tests can swap it after construction.
	h.persist = accounting.NewPersister(h.acct, func() time.Time { return h.now() })
	h.acct.SetOnChange(h.persist.Notify)
	h.acct.Configure(cfg.DailyBudgetUSD, time.Duration(cfg.CooldownSeconds)*time.Second)
	return h
}

// Handle runs one RPC call and returns the response envelope. It never panics: a panic inside the
// plugin becomes an error envelope (model.route is the exception, see route: a router that fails is
// skipped by the host, so it answers Handled:false instead).
func (h *Handler) Handle(method string, request []byte) (out []byte) {
	defer func() {
		if r := recover(); r != nil {
			h.noteError(fmt.Sprintf("%s: panic: %v", method, r))
			out = wire.ErrorEnvelope(wire.ErrorCodeInternal, fmt.Sprintf("%s: internal error", method))
		}
	}()
	switch method {
	case wire.MethodPluginRegister, wire.MethodPluginReconfigure:
		return h.lifecycle(request)
	case wire.MethodPluginQuiesce:
		return h.ok(struct{}{})
	case wire.MethodPluginShutdown:
		h.persist.Close()
		return h.ok(struct{}{})
	case wire.MethodModelRoute:
		return h.ok(h.route(request))
	case wire.MethodUsageHandle:
		return h.usage(request)
	case wire.MethodManagementRegister:
		return h.ok(wire.ManagementRegistrationResponse{
			Routes:    ManagementRoutes(),
			Resources: []wire.ResourceRoute{},
		})
	case wire.MethodManagementHandle:
		return h.ok(h.manage(request))
	default:
		return wire.ErrorEnvelope(wire.ErrorCodeUnknownMethod, "unknown method: "+method)
	}
}

func (h *Handler) ok(result any) []byte {
	raw, err := wire.OKEnvelope(result)
	if err != nil {
		h.noteError("encode response: " + err.Error())
		return wire.ErrorEnvelope(wire.ErrorCodeInternal, "encode response")
	}
	return raw
}

// lifecycle serves plugin.register and plugin.reconfigure: apply config_yaml, then publish the
// registration. A configuration that does not parse is not fatal: the previous good one stays in
// force and the error is kept for the management status.
func (h *Handler) lifecycle(request []byte) []byte {
	var req wire.LifecycleRequest
	if len(bytes.TrimSpace(request)) > 0 {
		if err := json.Unmarshal(request, &req); err != nil {
			return wire.ErrorEnvelope(wire.ErrorCodeBadRequest, "decode lifecycle request: "+err.Error())
		}
	}
	h.applyConfig(req.ConfigYAML)
	return h.ok(Registration())
}

func (h *Handler) applyConfig(raw []byte) {
	cfg, err := config.Parse(raw)
	h.mu.Lock()
	defer h.mu.Unlock()
	if err != nil {
		h.cfgErr = err
		return
	}
	h.cfgErr = nil
	if cfg.RouterFile != h.cfg.RouterFile {
		h.store = routerfile.NewStore(cfg.RouterFile)
	}
	h.acct.Configure(cfg.DailyBudgetUSD, time.Duration(cfg.CooldownSeconds)*time.Second)
	h.persist.SetPath(cfg.StatePath)
	h.decisions.SetKeep(cfg.DecisionsKeep)
	h.cfg = cfg
}

func (h *Handler) config() (config.Config, *routerfile.Store) {
	h.mu.RLock()
	defer h.mu.RUnlock()
	return h.cfg, h.store
}

func (h *Handler) noteError(msg string) {
	h.mu.Lock()
	h.lastErr, h.lastErrAt = msg, h.now()
	h.mu.Unlock()
}

// Registration is the plugin.register / plugin.reconfigure result.
func Registration() wire.Registration {
	return wire.Registration{
		SchemaVersion: wire.SchemaVersion,
		Metadata: wire.Metadata{
			Name:             PluginName,
			Version:          PluginVersion,
			Author:           PluginAuthor,
			GitHubRepository: PluginRepository,
			Logo:             "",
			ConfigFields:     ConfigFields(),
		},
		Capabilities: wire.Capabilities{ModelRouter: true, UsagePlugin: true, ManagementAPI: true},
	}
}

// ConfigFields describes the plugin's configuration keys (plugins.configs.tierbridge).
func ConfigFields() []wire.ConfigField {
	return []wire.ConfigField{
		{Name: "router-file", Type: wire.ConfigFieldString, Default: "",
			Description: "Path of tierbridge-router.json, the ranking exported by 'tierbridge export cpa-router'. Reloaded when the file changes; empty means the alias is never handled."},
		{Name: "alias", Type: wire.ConfigFieldString, Default: config.DefaultAlias,
			Description: "Model name the plugin answers to. Every other model is left to the host."},
		{Name: "daily-budget-usd", Type: wire.ConfigFieldNumber, Default: 0,
			Description: "Estimated spend allowed per UTC day, in USD. 0 means no budget."},
		{Name: "cooldown-seconds", Type: wire.ConfigFieldInteger, Default: config.DefaultCooldownSeconds,
			Description: "Seconds a candidate is skipped after an upstream failure reported by the usage plugin."},
		{Name: "state-path", Type: wire.ConfigFieldString, Default: "",
			Description: "File the quota counters, spend and cooldowns persist to across restarts (atomic JSON, mode 0600, written at most every 5 s). Empty means no persistence."},
		{Name: "decisions-keep", Type: wire.ConfigFieldInteger, Default: config.DefaultDecisionsKeep,
			Description: "How many recent routing decisions the management status keeps."},
		{Name: "debug-force-error", Type: wire.ConfigFieldBoolean, Default: false,
			Description: "TEST SWITCH ONLY: makes model routing fail internally so an acceptance run can prove the host falls through."},
	}
}

// Status is the plugin state for the management status.
type Status struct {
	Alias       string              `json:"alias"`
	Router      routerfile.Status   `json:"router"`
	ConfigError string              `json:"configError,omitempty"`
	LastError   string              `json:"lastError,omitempty"`
	LastErrorAt time.Time           `json:"lastErrorAt,omitzero"`
	Decisions   []selector.Decision `json:"decisions"`
	// LastUsageModels are the latest usage records seen, newest first.
	LastUsageModels []UsageSeen              `json:"lastUsageModels"`
	Persistence     accounting.PersistStatus `json:"persistence"`
}

// Status returns a snapshot of the plugin state. Router.Error and ConfigError hold the errors kept
// when the router file or the configuration were rejected.
func (h *Handler) Status() Status {
	cfg, store := h.config()
	h.mu.RLock()
	st := Status{Alias: cfg.Alias, LastError: h.lastErr, LastErrorAt: h.lastErrAt}
	if h.cfgErr != nil {
		st.ConfigError = h.cfgErr.Error()
	}
	h.mu.RUnlock()
	st.Router = store.Status()
	st.Decisions = h.decisions.Recent()
	st.LastUsageModels = h.usageSeen.recent()
	st.Persistence = h.persist.Status()
	return st
}

// route serves model.route. Anything that is not an alias request, and every internal failure, is
// answered with Handled:false so the host carries on with its normal model resolution.
func (h *Handler) route(request []byte) (resp wire.ModelRouteResponse) {
	var (
		alias     bool
		requested string
		est       estimate.Request
	)
	defer func() {
		if r := recover(); r != nil {
			msg := fmt.Sprintf("model.route: panic: %v", r)
			h.noteError(msg)
			resp = wire.ModelRouteResponse{Reason: "internal error"}
			if alias {
				h.record(requested, est, selector.Outcome{}, "internal error")
			}
		}
	}()

	cfg, store := h.config()
	requested = peekRequestedModel(request)
	if !strings.EqualFold(requested, cfg.Alias) {
		return wire.ModelRouteResponse{}
	}
	alias = true
	if cfg.DebugForceError {
		panic("debug-force-error is set")
	}

	var req wire.ModelRouteRequest
	if err := json.Unmarshal(request, &req); err != nil {
		return h.unhandled(requested, est, "undecodable route request: "+err.Error())
	}
	est = estimate.FromBody(req.Body)

	// A router file problem is not a routing error: the previous good version keeps serving, and the
	// problem stays visible in Status().Router.Error until the file is fixed.
	file, err := store.Current()
	if file == nil {
		reason := "no router file loaded"
		if err != nil {
			reason = "router file: " + err.Error()
		}
		return h.unhandled(requested, est, reason)
	}
	entry := file.Alias(cfg.Alias)
	if entry == nil {
		return h.unhandled(requested, est, fmt.Sprintf("router file has no alias %q", cfg.Alias))
	}

	now := h.now()
	out := selector.Select(est, entry.Candidates, req.AvailableProviders, h.acct, now)
	reason := out.Reason()
	h.record(requested, est, out, reason)
	if out.Picked == nil {
		return wire.ModelRouteResponse{Reason: reason}
	}
	return wire.ModelRouteResponse{
		Handled:     true,
		TargetKind:  wire.ModelRouteTargetProvider,
		Target:      out.Picked.Provider,
		TargetModel: out.Picked.Model,
		Reason:      reason,
	}
}

// unhandled answers Handled:false and records why in the decision ring.
func (h *Handler) unhandled(requested string, est estimate.Request, reason string) wire.ModelRouteResponse {
	h.record(requested, est, selector.Outcome{}, reason)
	return wire.ModelRouteResponse{Reason: reason}
}

func (h *Handler) record(requested string, est estimate.Request, out selector.Outcome, reason string) {
	d := selector.Decision{
		Time:           h.now(),
		RequestedModel: requested,
		EstTokens:      est.Tokens,
		Tools:          est.Tools,
		Image:          est.Image,
		Reason:         reason,
		Of:             out.Of,
		Skips:          out.Skips,
	}
	if out.Picked != nil {
		d.Handled, d.Provider, d.Model, d.Rank = true, out.Picked.Provider, out.Picked.Model, out.Rank
		d.Unpriced = out.Picked.Unpriced()
	}
	h.decisions.Add(d)
}

// peekRequestedModel reads "RequestedModel" from a model.route request without decoding the rest:
// the host serialises it before the (possibly large) body, so requests for other models are rejected
// after scanning only the first few fields.
func peekRequestedModel(request []byte) string {
	dec := json.NewDecoder(bytes.NewReader(request))
	tok, err := dec.Token()
	if err != nil || tok != json.Delim('{') {
		return ""
	}
	for dec.More() {
		keyTok, err := dec.Token()
		if err != nil {
			return ""
		}
		key, _ := keyTok.(string)
		if strings.EqualFold(key, "RequestedModel") {
			var model string
			if err := dec.Decode(&model); err != nil {
				return ""
			}
			return strings.TrimSpace(model)
		}
		var skip json.RawMessage
		if err := dec.Decode(&skip); err != nil {
			return ""
		}
	}
	return ""
}
