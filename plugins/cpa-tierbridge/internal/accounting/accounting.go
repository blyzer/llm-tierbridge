// Package accounting keeps the plugin's usage counters: per-model request and token windows, the
// daily spend against the budget, and the cooldown of models that failed upstream.
//
// Counters are keyed by the candidate's "model" (the name the plugin routes to). Requests are reserved
// at route time (Reserve, which implements selector.Limiter); tokens and spend are charged afterwards
// from the host's usage records (Charge). All times are explicit arguments, so tests drive the windows
// with an injected clock.
package accounting

import (
	"math"
	"net/http"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/estimate"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/selector"
)

const (
	// Window is the length of the sliding per-minute windows (rpm, tpm).
	Window = time.Minute
	// MaxRetryAfter bounds a Retry-After header: a hostile or broken upstream cannot park a model for days.
	MaxRetryAfter = 24 * time.Hour
	// maxTokens bounds one usage record's token count so sums cannot overflow.
	maxTokens = int64(1) << 40
)

// Usage is what the accountant needs from one host usage record.
type Usage struct {
	InputTokens, OutputTokens, TotalTokens int64
	Failed                                 bool
	StatusCode                             int
	// ResponseHeaders are the selected upstream response headers (Retry-After is the one read).
	ResponseHeaders map[string][]string
}

// Tokens is the token count charged against tpm/tpd: the total when reported, else input + output.
func (u Usage) Tokens() int64 {
	n := u.TotalTokens
	if n <= 0 {
		n = u.InputTokens + u.OutputTokens
	}
	return clamp(n)
}

func clamp(n int64) int64 {
	switch {
	case n < 0:
		return 0
	case n > maxTokens:
		return maxTokens
	}
	return n
}

type tokenEvent struct {
	at time.Time
	n  int64
}

type modelState struct {
	requests      []time.Time // reservations within the last Window, oldest first
	rpd           int
	tokens        []tokenEvent // charges within the last Window, oldest first
	tpd           int64
	cooldownUntil time.Time
}

// Accountant is safe for concurrent use.
type Accountant struct {
	mu       sync.Mutex
	budget   float64       // USD per UTC day; 0 = no budget
	cooldown time.Duration // base cooldown after a 429/5xx usage record
	day      string        // UTC day the daily counters belong to
	spent    float64
	unpriced map[string]bool // models charged today without a known price
	models   map[string]*modelState
	onChange func()
}

// New creates an empty accountant with no budget and no cooldown.
func New() *Accountant {
	return &Accountant{unpriced: map[string]bool{}, models: map[string]*modelState{}}
}

// Configure sets the daily budget in USD (0 = none) and the base cooldown.
func (a *Accountant) Configure(dailyBudgetUSD float64, cooldown time.Duration) {
	a.mu.Lock()
	a.budget, a.cooldown = dailyBudgetUSD, cooldown
	a.mu.Unlock()
}

// SetOnChange registers f, called (outside any accountant lock) after every change of the counters.
func (a *Accountant) SetOnChange(f func()) {
	a.mu.Lock()
	a.onChange = f
	a.mu.Unlock()
}

func (a *Accountant) notify() {
	a.mu.Lock()
	f := a.onChange
	a.mu.Unlock()
	if f != nil {
		f()
	}
}

func dayOf(t time.Time) string { return t.UTC().Format("2006-01-02") }

// rollLocked starts a new UTC day: daily counters and spend reset.
func (a *Accountant) rollLocked(now time.Time) {
	day := dayOf(now)
	if day == a.day {
		return
	}
	a.day, a.spent, a.unpriced = day, 0, map[string]bool{}
	for _, m := range a.models {
		m.rpd, m.tpd = 0, 0
	}
}

func (m *modelState) pruneLocked(now time.Time) {
	cutoff := now.Add(-Window)
	keep := m.requests[:0]
	for _, t := range m.requests {
		if t.After(cutoff) {
			keep = append(keep, t)
		}
	}
	m.requests = keep
	toks := m.tokens[:0]
	for _, e := range m.tokens {
		if e.at.After(cutoff) {
			toks = append(toks, e)
		}
	}
	m.tokens = toks
	if !m.cooldownUntil.After(now) {
		m.cooldownUntil = time.Time{}
	}
}

func (m *modelState) tpmLocked() int64 {
	var n int64
	for _, e := range m.tokens {
		n += e.n
	}
	return n
}

func (a *Accountant) stateLocked(model string) *modelState {
	m := a.models[model]
	if m == nil {
		m = &modelState{}
		a.models[model] = m
	}
	return m
}

// estimatedCost is the USD price of tokens prompt tokens on c; unknown prices count 0.
func estimatedCost(c routerfile.Candidate, tokens int) float64 {
	if c.Cost.Input == nil || tokens <= 0 {
		return 0
	}
	return float64(tokens) * *c.Cost.Input / 1e6
}

func isFree(c routerfile.Candidate) bool { return strings.EqualFold(c.Cost.Status, "free") }

// Reserve implements selector.Limiter. A candidate is refused while it cools down, when its estimated
// prompt cost would take the day's spend over the budget (free candidates are never refused for budget),
// when its rpm/rpd request quota is used up, or when the estimated prompt tokens do not fit the
// remaining tpm/tpd. Only when everything passes is one request consumed.
func (a *Accountant) Reserve(c routerfile.Candidate, req estimate.Request, now time.Time) (bool, selector.SkipReason) {
	ok, why := a.reserve(c, req, now)
	if ok {
		a.notify()
	}
	return ok, why
}

func (a *Accountant) reserve(c routerfile.Candidate, req estimate.Request, now time.Time) (bool, selector.SkipReason) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.rollLocked(now)
	m := a.stateLocked(c.Model)
	m.pruneLocked(now)

	if now.Before(m.cooldownUntil) {
		return false, selector.SkipCooldown
	}
	if a.budget > 0 && !isFree(c) && a.spent+estimatedCost(c, req.Tokens) > a.budget {
		return false, selector.SkipBudget
	}
	if c.RPM > 0 && len(m.requests) >= c.RPM {
		return false, selector.SkipRPM
	}
	if c.RPD > 0 && m.rpd >= c.RPD {
		return false, selector.SkipRPD
	}
	tokens := int64(max(req.Tokens, 0))
	if c.TPM > 0 && m.tpmLocked()+tokens > int64(c.TPM) {
		return false, selector.SkipTPM
	}
	if c.TPD > 0 && m.tpd+tokens > int64(c.TPD) {
		return false, selector.SkipTPD
	}
	m.requests = append(m.requests, now)
	m.rpd++
	return true, ""
}

// Charge books one usage record against candidate c: tokens into tpm/tpd, the price into the day's
// spend (a candidate without a known price counts 0 and is remembered as unpriced) and, for a failed
// record with status 429 or >= 500, a cooldown of at least the configured length (more when the
// upstream's Retry-After asks for it).
func (a *Accountant) Charge(c routerfile.Candidate, u Usage, now time.Time) {
	a.mu.Lock()
	a.rollLocked(now)
	m := a.stateLocked(c.Model)
	m.pruneLocked(now)

	if n := u.Tokens(); n > 0 {
		m.tokens = append(m.tokens, tokenEvent{at: now, n: n})
		m.tpd += n
	}
	if c.Unpriced() {
		a.unpriced[c.Model] = true
	} else if !isFree(c) {
		spend := (float64(clamp(u.InputTokens))**c.Cost.Input + float64(clamp(u.OutputTokens))**c.Cost.Output) / 1e6
		if spend > 0 && !math.IsInf(spend, 0) && !math.IsNaN(spend) {
			a.spent += spend
		}
	}
	if u.Failed && (u.StatusCode == http.StatusTooManyRequests || u.StatusCode >= 500) {
		d := max(a.cooldown, RetryAfter(u.ResponseHeaders, now))
		if until := now.Add(d); d > 0 && until.After(m.cooldownUntil) {
			m.cooldownUntil = until
		}
	}
	a.mu.Unlock()
	a.notify()
}

// Cool puts c on cooldown after an upstream failure seen while serving a request, the in-request twin
// of the cooldown Charge applies for a failed usage record: only 429 and >= 500 cool, for at least the
// configured length (more when the upstream's Retry-After asks for it). It reports whether the
// candidate now cools down.
func (a *Accountant) Cool(c routerfile.Candidate, status int, headers map[string][]string, now time.Time) bool {
	if status != http.StatusTooManyRequests && status < 500 {
		return false
	}
	a.mu.Lock()
	m := a.stateLocked(c.Model)
	d := max(a.cooldown, RetryAfter(headers, now))
	cooled := false
	if until := now.Add(d); d > 0 && until.After(m.cooldownUntil) {
		m.cooldownUntil = until
		cooled = true
	}
	a.mu.Unlock()
	if cooled {
		a.notify()
	}
	return cooled
}

// RetryAfter reads the Retry-After header (seconds or an HTTP date, any header-name case), bounded by
// MaxRetryAfter. It returns 0 when the header is absent or unusable.
func RetryAfter(headers map[string][]string, now time.Time) time.Duration {
	for name, values := range headers {
		if !strings.EqualFold(name, "Retry-After") {
			continue
		}
		for _, v := range values {
			v = strings.TrimSpace(v)
			var d time.Duration
			if secs, err := strconv.ParseFloat(v, 64); err == nil && !math.IsNaN(secs) {
				if secs > MaxRetryAfter.Seconds() {
					return MaxRetryAfter
				}
				d = time.Duration(secs * float64(time.Second))
			} else if at, err := http.ParseTime(v); err == nil {
				d = at.Sub(now)
			}
			if d > MaxRetryAfter {
				d = MaxRetryAfter
			}
			if d > 0 {
				return d
			}
		}
	}
	return 0
}

// Counter is one windowed counter: Limit nil means unlimited.
type Counter struct {
	Used  int64  `json:"used"`
	Limit *int64 `json:"limit"`
}

// ModelCounters is the counter view of one candidate.
type ModelCounters struct {
	Model string  `json:"model"`
	RPM   Counter `json:"rpm"`
	RPD   Counter `json:"rpd"`
	TPM   Counter `json:"tpm"`
	TPD   Counter `json:"tpd"`
	// CooldownRemainingSeconds is 0 when the model is not cooling down.
	CooldownRemainingSeconds int64 `json:"cooldownRemainingSeconds"`
}

func limitOf(n int) *int64 {
	if n <= 0 {
		return nil
	}
	v := int64(n)
	return &v
}

// Counters returns the counters of cands, in order, as of now.
func (a *Accountant) Counters(cands []routerfile.Candidate, now time.Time) []ModelCounters {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.rollLocked(now)
	out := make([]ModelCounters, 0, len(cands))
	for _, c := range cands {
		mc := ModelCounters{Model: c.Model,
			RPM: Counter{Limit: limitOf(c.RPM)}, RPD: Counter{Limit: limitOf(c.RPD)},
			TPM: Counter{Limit: limitOf(c.TPM)}, TPD: Counter{Limit: limitOf(c.TPD)}}
		if m := a.models[c.Model]; m != nil {
			m.pruneLocked(now)
			mc.RPM.Used, mc.RPD.Used = int64(len(m.requests)), int64(m.rpd)
			mc.TPM.Used, mc.TPD.Used = m.tpmLocked(), m.tpd
			if m.cooldownUntil.After(now) {
				mc.CooldownRemainingSeconds = int64(math.Ceil(m.cooldownUntil.Sub(now).Seconds()))
			}
		}
		out = append(out, mc)
	}
	return out
}

// Budget is the budget view for the management API.
type Budget struct {
	Day            string  `json:"day"`
	DailyBudgetUSD float64 `json:"dailyBudgetUsd"`
	SpentUSD       float64 `json:"spentUsd"`
	// RemainingUSD is nil when no budget is set.
	RemainingUSD *float64 `json:"remainingUsd"`
	// Unpriced lists the models whose price is unknown: budget math counts them as 0.
	Unpriced []string `json:"unpriced"`
}

// Budget returns the day's spend against the budget. unpriced are further models (the unpriced
// candidates of the loaded router file) merged into the listing.
func (a *Accountant) Budget(now time.Time, unpriced []string) Budget {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.rollLocked(now)
	b := Budget{Day: a.day, DailyBudgetUSD: a.budget, SpentUSD: a.spent, Unpriced: []string{}}
	if a.budget > 0 {
		rem := math.Max(a.budget-a.spent, 0)
		b.RemainingUSD = &rem
	}
	set := make(map[string]bool, len(a.unpriced)+len(unpriced))
	for m := range a.unpriced {
		set[m] = true
	}
	for _, m := range unpriced {
		set[m] = true
	}
	for m := range set {
		b.Unpriced = append(b.Unpriced, m)
	}
	sort.Strings(b.Unpriced)
	return b
}

// Spent returns the spend of the UTC day containing now.
func (a *Accountant) Spent(now time.Time) float64 {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.rollLocked(now)
	return a.spent
}
