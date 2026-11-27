package rpc

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/accounting"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/selector"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

// Management API paths (relative to the host's management base path).
const (
	PathStatus     = "/tierbridge/status"
	PathCandidates = "/tierbridge/candidates"
	PathCounters   = "/tierbridge/counters"
	PathBudget     = "/tierbridge/budget"
	PathDecisions  = "/tierbridge/decisions"
)

// ManagementRoutes is what management.register publishes: read-only GET routes, no menu entries.
func ManagementRoutes() []wire.ManagementRoute {
	return []wire.ManagementRoute{
		{Method: "GET", Path: PathStatus, Description: "tierbridge: plugin version, router file revision, configuration, persistence and the latest usage records seen"},
		{Method: "GET", Path: PathCandidates, Description: "tierbridge: the ranked candidates with their limits"},
		{Method: "GET", Path: PathCounters, Description: "tierbridge: per-model rpm/rpd/tpm/tpd used against the limits and cooldown remaining"},
		{Method: "GET", Path: PathBudget, Description: "tierbridge: spend today against the daily budget and the unpriced models"},
		{Method: "GET", Path: PathDecisions, Description: "tierbridge: the last routing decisions (query: limit=N)"},
	}
}

// managementRequest is the part of rpcManagementRequest the plugin reads. The host's headers and body
// are deliberately not decoded, so they can never end up in a response.
type managementRequest struct {
	Method string
	Path   string
	Query  map[string][]string
}

func jsonResponse(status int, v any) wire.ManagementResponse {
	body, err := wire.Marshal(v)
	if err != nil {
		status, body = http.StatusInternalServerError, []byte(`{"error":"encode response"}`)
	}
	return wire.ManagementResponse{
		StatusCode: status,
		Headers:    map[string][]string{"Content-Type": {"application/json"}},
		Body:       body,
	}
}

func errorResponse(status int, msg string) wire.ManagementResponse {
	return jsonResponse(status, map[string]string{"error": msg})
}

// notFound is the 404 JSON answer of every unknown management path.
func notFound() wire.ManagementResponse { return errorResponse(http.StatusNotFound, "not found") }

// manage serves management.handle. A route that panics answers 500 JSON instead of failing the call.
func (h *Handler) manage(request []byte) (resp wire.ManagementResponse) {
	defer func() {
		if r := recover(); r != nil {
			h.noteError(fmt.Sprintf("management.handle: panic: %v", r))
			resp = errorResponse(http.StatusInternalServerError, "internal error")
		}
	}()
	var req managementRequest
	if len(bytes.TrimSpace(request)) > 0 {
		_ = json.Unmarshal(request, &req) // a malformed request reads as an empty one: 404
	}
	path, _, _ := strings.Cut(req.Path, "?")
	path = strings.TrimRight(path, "/")
	route := matchRoute(path)
	if route == "" {
		return notFound()
	}
	if m := strings.ToUpper(strings.TrimSpace(req.Method)); m != "" && m != http.MethodGet && m != http.MethodHead {
		resp := errorResponse(http.StatusMethodNotAllowed, "method not allowed")
		resp.Headers["Allow"] = []string{"GET, HEAD"}
		return resp
	}
	if h.beforeManagement != nil {
		h.beforeManagement(route)
	}
	switch route {
	case PathStatus:
		return jsonResponse(http.StatusOK, h.statusView())
	case PathCandidates:
		return jsonResponse(http.StatusOK, h.candidatesView())
	case PathCounters:
		return jsonResponse(http.StatusOK, h.countersView())
	case PathBudget:
		return jsonResponse(http.StatusOK, h.budgetView())
	default:
		return h.decisionsResponse(req.Query)
	}
}

// matchRoute maps a request path to a route. The host may hand over the path with its management base
// path in front, so the route is matched as a suffix on a path-segment boundary.
func matchRoute(path string) string {
	for _, r := range []string{PathStatus, PathCandidates, PathCounters, PathBudget, PathDecisions} {
		if path == r || strings.HasSuffix(path, r) {
			return r
		}
	}
	return ""
}

// ---- status -------------------------------------------------------------------------------------

type pluginView struct {
	Name          string `json:"name"`
	Version       string `json:"version"`
	ABIVersion    uint32 `json:"abiVersion"`
	SchemaVersion uint32 `json:"schemaVersion"`
	HostCommit    string `json:"hostCommit"`
}

type revisionView struct {
	CatalogSHA256     string    `json:"catalogSha256"`
	QuotaSourceSHA256 string    `json:"quotaSourceSha256"`
	FileSHA256        string    `json:"fileSha256"`
	ModTime           time.Time `json:"modTime"`
	Profile           string    `json:"profile"`
	LoadedAt          time.Time `json:"loadedAt"`
}

type routerView struct {
	Path     string                   `json:"path"`
	Revision revisionView             `json:"revision"`
	Aliases  []routerfile.AliasStatus `json:"aliases"`
	// LastLoadError is empty when the router file is healthy.
	LastLoadError string `json:"lastLoadError"`
}

type configView struct {
	RouterFile      string  `json:"routerFile"`
	Alias           string  `json:"alias"`
	DailyBudgetUSD  float64 `json:"dailyBudgetUsd"`
	CooldownSeconds int     `json:"cooldownSeconds"`
	StatePath       string  `json:"statePath"`
	DecisionsKeep   int     `json:"decisionsKeep"`
	DebugForceError bool    `json:"debugForceError"`
}

type statusDoc struct {
	Plugin          pluginView               `json:"plugin"`
	Router          routerView               `json:"router"`
	Config          configView               `json:"config"`
	ConfigError     string                   `json:"configError"`
	LastError       string                   `json:"lastError"`
	LastErrorAt     *time.Time               `json:"lastErrorAt"`
	LastUsageModels []UsageSeen              `json:"lastUsageModels"`
	Persistence     accounting.PersistStatus `json:"persistence"`
}

func (h *Handler) statusView() statusDoc {
	st := h.Status()
	cfg, _ := h.config()
	rs := st.Router
	doc := statusDoc{
		Plugin: pluginView{Name: PluginName, Version: PluginVersion, ABIVersion: wire.ABIVersion,
			SchemaVersion: wire.SchemaVersion, HostCommit: wire.HostCommit},
		Router: routerView{Path: rs.Path, Aliases: rs.Aliases, LastLoadError: rs.Error,
			Revision: revisionView{
				CatalogSHA256: rs.Revision.CatalogSHA256, QuotaSourceSHA256: rs.Revision.QuotaSourceSHA256,
				FileSHA256: rs.Revision.FileSHA256, ModTime: rs.Revision.ModTime, LoadedAt: rs.LoadedAt,
			}},
		Config: configView{RouterFile: cfg.RouterFile, Alias: cfg.Alias, DailyBudgetUSD: cfg.DailyBudgetUSD,
			CooldownSeconds: cfg.CooldownSeconds, StatePath: cfg.StatePath, DecisionsKeep: cfg.DecisionsKeep,
			DebugForceError: cfg.DebugForceError},
		ConfigError:     st.ConfigError,
		LastError:       st.LastError,
		LastUsageModels: st.LastUsageModels,
		Persistence:     st.Persistence,
	}
	if !st.LastErrorAt.IsZero() {
		at := st.LastErrorAt
		doc.LastErrorAt = &at
	}
	for _, a := range rs.Aliases {
		if strings.EqualFold(a.Alias, cfg.Alias) {
			doc.Router.Revision.Profile = a.Profile
		}
	}
	if doc.Router.Aliases == nil {
		doc.Router.Aliases = []routerfile.AliasStatus{}
	}
	if doc.LastUsageModels == nil {
		doc.LastUsageModels = []UsageSeen{}
	}
	return doc
}

// ---- candidates, counters, budget ----------------------------------------------------------------

// activeCandidates returns the configured alias's candidates (empty when no router file is usable).
func (h *Handler) activeCandidates() (alias, profile string, cands []routerfile.Candidate) {
	cfg, store := h.config()
	file, _ := store.Current()
	if entry := file.Alias(cfg.Alias); entry != nil {
		return cfg.Alias, entry.Profile, entry.Candidates
	}
	return cfg.Alias, "", nil
}

type limitsView struct {
	RPM *int `json:"rpm"`
	RPD *int `json:"rpd"`
	TPM *int `json:"tpm"`
	TPD *int `json:"tpd"`
}

type costView struct {
	Status string   `json:"status"`
	Input  *float64 `json:"input"`
	Output *float64 `json:"output"`
}

type candidateView struct {
	Rank          int        `json:"rank"`
	Model         string     `json:"model"`
	ID            string     `json:"id"`
	ContextWindow int        `json:"contextWindow"`
	MaxTokens     int        `json:"maxTokens"`
	SupportsTools bool       `json:"supportsTools"`
	Input         []string   `json:"input"`
	Cost          costView   `json:"cost"`
	Unpriced      bool       `json:"unpriced"`
	Limits        limitsView `json:"limits"`
}

type candidatesDoc struct {
	Alias      string          `json:"alias"`
	Profile    string          `json:"profile"`
	Candidates []candidateView `json:"candidates"`
}

func orNil(n int) *int {
	if n <= 0 {
		return nil
	}
	return &n
}

func (h *Handler) candidatesView() candidatesDoc {
	alias, profile, cands := h.activeCandidates()
	doc := candidatesDoc{Alias: alias, Profile: profile, Candidates: make([]candidateView, 0, len(cands))}
	for i, c := range cands {
		in := c.Input
		if in == nil {
			in = []string{}
		}
		doc.Candidates = append(doc.Candidates, candidateView{
			Rank: i + 1, Model: c.Model, ID: c.ID,
			ContextWindow: c.ContextWindow, MaxTokens: c.MaxTokens, SupportsTools: c.SupportsTools,
			Input:    in,
			Cost:     costView{Status: c.Cost.Status, Input: c.Cost.Input, Output: c.Cost.Output},
			Unpriced: c.Unpriced(),
			Limits:   limitsView{RPM: orNil(c.RPM), RPD: orNil(c.RPD), TPM: orNil(c.TPM), TPD: orNil(c.TPD)},
		})
	}
	return doc
}

type countersDoc struct {
	Day    string                     `json:"day"`
	Now    time.Time                  `json:"now"`
	Models []accounting.ModelCounters `json:"models"`
}

func (h *Handler) countersView() countersDoc {
	_, _, cands := h.activeCandidates()
	now := h.now()
	return countersDoc{Day: now.UTC().Format("2006-01-02"), Now: now.UTC(), Models: h.acct.Counters(cands, now)}
}

func (h *Handler) budgetView() accounting.Budget {
	_, _, cands := h.activeCandidates()
	var unpriced []string
	for _, c := range cands {
		if c.Unpriced() {
			unpriced = append(unpriced, c.Model)
		}
	}
	return h.acct.Budget(h.now(), unpriced)
}

// ---- decisions ----------------------------------------------------------------------------------

type chosenView struct {
	Model string `json:"model"`
	Rank  int    `json:"rank"`
	// Status is the HTTP status of the answer returned to the client.
	Status int `json:"status"`
}

type decisionView struct {
	Time            time.Time       `json:"time"`
	RequestedModel  string          `json:"requestedModel"`
	Chosen          *chosenView     `json:"chosen"`
	Reason          string          `json:"reason"`
	Skipped         []selector.Skip `json:"skipped"`
	EstimatedTokens int             `json:"estimatedTokens"`
	// Attempts are the host.model.execute calls made for the request, in order; empty for a request
	// the plugin left to the host.
	Attempts []selector.Attempt `json:"attempts"`
	// Status is the HTTP status of the final answer of an executed request (0 when not executed).
	Status int `json:"status"`
}

type decisionsDoc struct {
	Limit     int            `json:"limit"`
	Decisions []decisionView `json:"decisions"`
}

// defaultDecisionsLimit is how many decisions /tierbridge/decisions returns without a limit parameter.
const defaultDecisionsLimit = 20

func (h *Handler) decisionsResponse(query map[string][]string) wire.ManagementResponse {
	limit := defaultDecisionsLimit
	if vals := query["limit"]; len(vals) > 0 {
		n, err := strconv.Atoi(strings.TrimSpace(vals[0]))
		if err != nil || n < 1 {
			return errorResponse(http.StatusBadRequest, "limit must be a positive integer")
		}
		limit = n
	}
	recent := h.decisions.Recent() // newest first
	if len(recent) > limit {
		recent = recent[:limit]
	}
	doc := decisionsDoc{Limit: limit, Decisions: make([]decisionView, 0, len(recent))}
	for _, d := range recent {
		v := decisionView{Time: d.Time, RequestedModel: d.RequestedModel, Reason: d.Reason,
			Skipped: d.Skips, EstimatedTokens: d.EstTokens, Attempts: d.Attempts, Status: d.Status}
		if v.Skipped == nil {
			v.Skipped = []selector.Skip{}
		}
		if v.Attempts == nil {
			v.Attempts = []selector.Attempt{}
		}
		if n := len(d.Attempts); n > 0 && d.Attempts[n-1].OK {
			v.Chosen = &chosenView{Model: d.Model, Rank: d.Rank, Status: d.Status}
		}
		doc.Decisions = append(doc.Decisions, v)
	}
	return jsonResponse(http.StatusOK, doc)
}
