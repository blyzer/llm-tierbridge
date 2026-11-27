package rpc

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/accounting"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
)

// usageRecord is the part of pluginapi.UsageRecord (sdk/pluginapi/types.go: capitalised keys, no json
// tags, the record is the whole usage.handle request) the plugin reads.
type usageRecord struct {
	Provider string
	Model    string
	Alias    string
	Failed   bool
	Failure  struct {
		StatusCode int
	}
	Detail struct {
		InputTokens  int64
		OutputTokens int64
		TotalTokens  int64
	}
	ResponseHeaders map[string][]string
}

// MaxUsageSeen is the size of the lastUsageModels ring.
const MaxUsageSeen = 20

// UsageSeen is one usage record as the plugin saw it, kept so an operator can check whether the host's
// UsageRecord.Model equals the TargetModel the plugin routed to.
type UsageSeen struct {
	Time     time.Time `json:"time"`
	Model    string    `json:"model"`
	Alias    string    `json:"alias"`
	Provider string    `json:"provider"`
	Failed   bool      `json:"failed"`
	Status   int       `json:"status"`
	Tokens   int64     `json:"tokens"`
	// Matched reports that the record was matched to a candidate and charged.
	Matched bool `json:"matched"`
}

// usageRing keeps the last MaxUsageSeen records, newest first on read.
type usageRing struct {
	mu   sync.Mutex
	buf  []UsageSeen
	next int
	n    int
}

func (r *usageRing) add(u UsageSeen) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.buf == nil {
		r.buf = make([]UsageSeen, MaxUsageSeen)
	}
	r.buf[r.next] = u
	r.next = (r.next + 1) % MaxUsageSeen
	r.n = min(r.n+1, MaxUsageSeen)
}

func (r *usageRing) recent() []UsageSeen {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make([]UsageSeen, 0, r.n)
	for i := 1; i <= r.n; i++ {
		out = append(out, r.buf[(r.next-i+2*MaxUsageSeen)%MaxUsageSeen])
	}
	return out
}

// usage serves usage.handle. It always answers success: the host ignores the result, and a usage
// record the plugin cannot make sense of must never turn into an error on the host's side.
func (h *Handler) usage(request []byte) (out []byte) {
	defer func() {
		if r := recover(); r != nil {
			h.noteError(fmt.Sprintf("usage.handle: panic: %v", r))
			out = h.ok(struct{}{})
		}
	}()
	h.handleUsage(request)
	return h.ok(struct{}{})
}

func (h *Handler) handleUsage(request []byte) {
	var rec usageRecord
	if err := json.Unmarshal(request, &rec); err != nil {
		// A field of the wrong type still leaves the other fields decoded; broken JSON leaves nothing.
		var typeErr *json.UnmarshalTypeError
		if !errors.As(err, &typeErr) {
			return
		}
	}
	now := h.now()
	u := accounting.Usage{
		InputTokens:     rec.Detail.InputTokens,
		OutputTokens:    rec.Detail.OutputTokens,
		TotalTokens:     rec.Detail.TotalTokens,
		Failed:          rec.Failed,
		StatusCode:      rec.Failure.StatusCode,
		ResponseHeaders: rec.ResponseHeaders,
	}
	seen := UsageSeen{
		Time: now, Model: rec.Model, Alias: rec.Alias, Provider: rec.Provider,
		Failed: rec.Failed, Status: rec.Failure.StatusCode, Tokens: u.Tokens(),
	}
	if c := h.matchCandidate(rec); c != nil {
		h.acct.Charge(*c, u, now)
		seen.Matched = true
	}
	h.usageSeen.add(seen)
}

// matchCandidate finds the candidate of the configured alias a usage record belongs to. Candidates
// carry no provider (router file /2: the host resolves the model), so the record's model is all there is
// to go by. Stages, first hit wins: Model == candidate.model, Alias == candidate.model (both exact, then
// case-insensitive), then Model == candidate.id (the host reports the upstream id, without the "tb-"
// prefix of the exported model name). A stage with several hits is ambiguous and matches nothing. The
// record of the executor's own call (model = the alias, provider "tierbridge") matches no candidate.
func (h *Handler) matchCandidate(rec usageRecord) *routerfile.Candidate {
	cfg, store := h.config()
	file, _ := store.Current()
	entry := file.Alias(cfg.Alias)
	if entry == nil {
		return nil
	}
	model, alias := strings.TrimSpace(rec.Model), strings.TrimSpace(rec.Alias)
	byModel := func(c *routerfile.Candidate, name string) bool { return c.Model == name }
	byModelFold := func(c *routerfile.Candidate, name string) bool { return strings.EqualFold(c.Model, name) }
	byID := func(c *routerfile.Candidate, name string) bool { return c.ID == name }
	byIDFold := func(c *routerfile.Candidate, name string) bool { return strings.EqualFold(c.ID, name) }
	type stage struct {
		name string
		eq   func(*routerfile.Candidate, string) bool
	}
	for _, st := range []stage{
		{model, byModel}, {alias, byModel}, {model, byModelFold}, {alias, byModelFold}, {model, byID}, {model, byIDFold},
	} {
		if st.name == "" {
			continue
		}
		var hits []*routerfile.Candidate
		for i := range entry.Candidates {
			if c := &entry.Candidates[i]; st.eq(c, st.name) {
				hits = append(hits, c)
			}
		}
		switch len(hits) {
		case 1:
			return hits[0]
		case 0:
			continue
		default:
			return nil // ambiguous
		}
	}
	return nil
}
