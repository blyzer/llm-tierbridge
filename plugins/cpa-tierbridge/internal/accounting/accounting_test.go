package accounting

import (
	"net/http"
	"testing"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/estimate"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/selector"
)

var t0 = time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)

func free(model string) routerfile.Candidate {
	return routerfile.Candidate{Provider: "p", Model: model, Cost: routerfile.Cost{Status: "free", Input: new(0.0), Output: new(0.0)}}
}

// paid is priced at $1 per million input tokens and $2 per million output tokens.
func paid(model string) routerfile.Candidate {
	return routerfile.Candidate{Provider: "p", Model: model, Cost: routerfile.Cost{Status: "known", Input: new(1.0), Output: new(2.0)}}
}

func unknown(model string) routerfile.Candidate {
	return routerfile.Candidate{Provider: "p", Model: model, Cost: routerfile.Cost{Status: "unknown"}}
}

func req(tokens int) estimate.Request { return estimate.Request{Tokens: tokens} }

func mustReserve(t *testing.T, a *Accountant, c routerfile.Candidate, tokens int, now time.Time) {
	t.Helper()
	if ok, why := a.Reserve(c, req(tokens), now); !ok {
		t.Fatalf("Reserve(%s, %d tokens) refused: %s", c.Model, tokens, why)
	}
}

func mustRefuse(t *testing.T, a *Accountant, c routerfile.Candidate, tokens int, now time.Time, want selector.SkipReason) {
	t.Helper()
	if ok, why := a.Reserve(c, req(tokens), now); ok || why != want {
		t.Fatalf("Reserve(%s, %d tokens) = %v, %q; want refused with %q", c.Model, tokens, ok, why, want)
	}
}

func TestRPMWindowExpiry(t *testing.T) {
	a := New()
	c := free("m")
	c.RPM = 2
	mustReserve(t, a, c, 0, t0)
	mustReserve(t, a, c, 0, t0.Add(30*time.Second))
	mustRefuse(t, a, c, 0, t0.Add(59*time.Second), selector.SkipRPM)
	// A refusal consumed nothing: when the first request leaves the window exactly one slot opens.
	mustReserve(t, a, c, 0, t0.Add(61*time.Second))
	mustRefuse(t, a, c, 0, t0.Add(62*time.Second), selector.SkipRPM)
	// Both remaining requests leave the window.
	mustReserve(t, a, c, 0, t0.Add(125*time.Second))
}

func TestRPDResetsAtUTCMidnight(t *testing.T) {
	a := New()
	c := free("m")
	c.RPD = 2
	evening := time.Date(2026, 11, 27, 23, 58, 0, 0, time.UTC)
	mustReserve(t, a, c, 0, evening)
	mustReserve(t, a, c, 0, evening.Add(30*time.Second))
	mustRefuse(t, a, c, 0, evening.Add(60*time.Second), selector.SkipRPD)
	mustReserve(t, a, c, 0, time.Date(2026, 11, 28, 0, 0, 0, 0, time.UTC))
	// The day is UTC whatever the zone of the clock: 19:30 at UTC-5 on the 27th is 00:30 UTC on the 28th.
	zone := time.FixedZone("x", -5*3600)
	mustReserve(t, a, c, 0, time.Date(2026, 11, 27, 19, 30, 0, 0, zone))
	mustRefuse(t, a, c, 0, time.Date(2026, 11, 27, 19, 31, 0, 0, zone), selector.SkipRPD)
}

func TestTokenWindowsAndEstimateMustFit(t *testing.T) {
	a := New()
	c := free("m")
	c.TPM, c.TPD = 1000, 1500
	mustReserve(t, a, c, 500, t0) // the estimate alone is not charged: only usage records are
	a.Charge(c, Usage{TotalTokens: 800}, t0)
	mustRefuse(t, a, c, 300, t0.Add(10*time.Second), selector.SkipTPM) // 800 + 300 > 1000
	mustReserve(t, a, c, 200, t0.Add(10*time.Second))                  // 800 + 200 fits exactly
	mustReserve(t, a, c, 700, t0.Add(61*time.Second))                  // the minute window slid past the 800
	// tpd: 800 charged today, 700 more charged inside the next minutes.
	a.Charge(c, Usage{InputTokens: 300, OutputTokens: 400}, t0.Add(2*time.Minute)) // total missing: input + output
	mustRefuse(t, a, c, 100, t0.Add(10*time.Minute), selector.SkipTPD)             // 1500 + 100 > 1500
	mustReserve(t, a, c, 0, t0.Add(10*time.Minute))
	// Next UTC day: the daily token counter starts again.
	mustReserve(t, a, c, 900, time.Date(2026, 11, 28, 0, 0, 1, 0, time.UTC))
}

func TestNullLimitsAreUnlimited(t *testing.T) {
	a := New()
	c := free("m") // RPM, RPD, TPM, TPD all 0
	for range 100 {
		mustReserve(t, a, c, 1_000_000, t0)
		a.Charge(c, Usage{TotalTokens: 1_000_000}, t0)
	}
}

func TestBudgetSkipsPaidButNeverFree(t *testing.T) {
	a := New()
	a.Configure(1, 0) // $1 per UTC day
	p, f := paid("p"), free("f")

	// Estimated prompt cost: 500k tokens at $1/M = $0.50.
	mustReserve(t, a, p, 500_000, t0)
	a.Charge(p, Usage{InputTokens: 800_000, OutputTokens: 100_000}, t0) // 0.8 + 0.2 = $1.00 exactly
	if got := a.Spent(t0); got < 0.999999 || got > 1.000001 {
		t.Fatalf("spent = %v, want 1.0", got)
	}
	mustRefuse(t, a, p, 500_000, t0, selector.SkipBudget)
	mustReserve(t, a, p, 0, t0) // costs nothing: spend + 0 does not exceed the budget
	mustReserve(t, a, f, 500_000_000, t0)
	a.Charge(f, Usage{InputTokens: 500_000_000, OutputTokens: 500_000_000}, t0)
	if got := a.Spent(t0); got > 1.000001 {
		t.Errorf("a free model changed the spend: %v", got)
	}
	// The spend belongs to a UTC day.
	mustReserve(t, a, p, 500_000, time.Date(2026, 11, 28, 0, 0, 0, 0, time.UTC))
}

func TestBudgetWouldExceedUsesEstimatedPromptCost(t *testing.T) {
	a := New()
	a.Configure(1, 0)
	p := paid("p")
	a.Charge(p, Usage{InputTokens: 600_000}, t0) // $0.60
	mustRefuse(t, a, p, 500_000, t0, selector.SkipBudget)
	mustReserve(t, a, p, 400_000, t0) // 0.60 + 0.40 = 1.00 is not over
}

func TestUnpricedCountsZeroAndIsListed(t *testing.T) {
	a := New()
	a.Configure(1, 0)
	u := unknown("u")
	a.Charge(u, Usage{InputTokens: 9_000_000, OutputTokens: 9_000_000}, t0)
	if got := a.Spent(t0); got != 0 {
		t.Errorf("spent = %v, want 0 for an unpriced model", got)
	}
	mustReserve(t, a, u, 9_000_000, t0) // est cost 0: never skipped while spend is within budget
	b := a.Budget(t0, []string{"from-file"})
	if len(b.Unpriced) != 2 || b.Unpriced[0] != "from-file" || b.Unpriced[1] != "u" {
		t.Errorf("unpriced = %v", b.Unpriced)
	}
	if b.RemainingUSD == nil || *b.RemainingUSD != 1 {
		t.Errorf("remaining = %v", b.RemainingUSD)
	}
}

func TestNoBudgetMeansNoRemaining(t *testing.T) {
	b := New().Budget(t0, nil)
	if b.RemainingUSD != nil || b.DailyBudgetUSD != 0 || b.Unpriced == nil {
		t.Errorf("budget = %+v", b)
	}
}

func TestCooldownFromA429UsageRecord(t *testing.T) {
	a := New()
	a.Configure(0, 90*time.Second)
	c := free("m")
	other := free("other")
	a.Charge(c, Usage{Failed: true, StatusCode: http.StatusTooManyRequests}, t0)
	mustRefuse(t, a, c, 0, t0.Add(89*time.Second), selector.SkipCooldown)
	mustReserve(t, a, other, 0, t0.Add(time.Second)) // other models are untouched
	mustReserve(t, a, c, 0, t0.Add(91*time.Second))
	mc := a.Counters([]routerfile.Candidate{c}, t0.Add(10*time.Second))
	if mc[0].CooldownRemainingSeconds != 0 {
		// reserved at +91 s, so the cooldown is long over: remaining must be 0
		t.Errorf("cooldown remaining = %d", mc[0].CooldownRemainingSeconds)
	}
}

func TestCooldownRemainingSeconds(t *testing.T) {
	a := New()
	a.Configure(0, 60*time.Second)
	c := free("m")
	a.Charge(c, Usage{Failed: true, StatusCode: 503}, t0)
	got := a.Counters([]routerfile.Candidate{c}, t0.Add(20*time.Second))[0].CooldownRemainingSeconds
	if got != 40 {
		t.Errorf("remaining = %d, want 40", got)
	}
}

func TestCooldownUsesTheLargerRetryAfter(t *testing.T) {
	headers := func(v string) map[string][]string { return map[string][]string{"retry-after": {v}} }
	tests := []struct {
		name     string
		cooldown time.Duration
		headers  map[string][]string
		want     time.Duration
	}{
		{"retry-after larger", 60 * time.Second, headers("300"), 300 * time.Second},
		{"configured larger", 60 * time.Second, headers("5"), 60 * time.Second},
		{"http date", 10 * time.Second, headers(t0.Add(2 * time.Minute).UTC().Format(http.TimeFormat)), 2 * time.Minute},
		{"no cooldown configured", 0, headers("45"), 45 * time.Second},
		{"garbage header", 30 * time.Second, headers("soon"), 30 * time.Second},
		{"absurd value bounded", 30 * time.Second, headers("99999999999"), MaxRetryAfter},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			a := New()
			a.Configure(0, tc.cooldown)
			c := free("m")
			a.Charge(c, Usage{Failed: true, StatusCode: 429, ResponseHeaders: tc.headers}, t0)
			mustRefuse(t, a, c, 0, t0.Add(tc.want-time.Second), selector.SkipCooldown)
			mustReserve(t, a, c, 0, t0.Add(tc.want+time.Second))
		})
	}
}

func TestOnlyFailed429And5xxCoolDown(t *testing.T) {
	for _, tc := range []struct {
		name string
		u    Usage
		cool bool
	}{
		{"429", Usage{Failed: true, StatusCode: 429}, true},
		{"500", Usage{Failed: true, StatusCode: 500}, true},
		{"599", Usage{Failed: true, StatusCode: 599}, true},
		{"400", Usage{Failed: true, StatusCode: 400}, false},
		{"404", Usage{Failed: true, StatusCode: 404}, false},
		{"no status", Usage{Failed: true}, false},
		{"429 but not failed", Usage{StatusCode: 429}, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			a := New()
			a.Configure(0, time.Minute)
			c := free("m")
			a.Charge(c, tc.u, t0)
			ok, _ := a.Reserve(c, req(0), t0.Add(time.Second))
			if ok == tc.cool {
				t.Errorf("reserve ok = %v, want %v", ok, !tc.cool)
			}
		})
	}
}

func TestRefusalConsumesNothing(t *testing.T) {
	a := New()
	c := free("m")
	c.RPM, c.TPM = 5, 100
	for range 10 {
		mustRefuse(t, a, c, 1000, t0, selector.SkipTPM)
	}
	got := a.Counters([]routerfile.Candidate{c}, t0)[0]
	if got.RPM.Used != 0 || got.RPD.Used != 0 {
		t.Errorf("refused reserves consumed requests: %+v", got)
	}
}

func TestCountersShape(t *testing.T) {
	a := New()
	c := free("m")
	c.RPM, c.TPD = 10, 5000
	mustReserve(t, a, c, 0, t0)
	a.Charge(c, Usage{TotalTokens: 42}, t0)
	got := a.Counters([]routerfile.Candidate{c}, t0)[0]
	if got.RPM.Used != 1 || *got.RPM.Limit != 10 || got.RPD.Limit != nil || got.TPM.Used != 42 || got.TPM.Limit != nil ||
		got.TPD.Used != 42 || *got.TPD.Limit != 5000 {
		t.Errorf("counters = %+v", got)
	}
}

func TestUsageTokens(t *testing.T) {
	for _, tc := range []struct {
		u    Usage
		want int64
	}{
		{Usage{TotalTokens: 10, InputTokens: 1, OutputTokens: 1}, 10},
		{Usage{InputTokens: 3, OutputTokens: 4}, 7},
		{Usage{TotalTokens: -5}, 0},
		{Usage{TotalTokens: 1 << 62}, 1 << 40},
	} {
		if got := tc.u.Tokens(); got != tc.want {
			t.Errorf("%+v.Tokens() = %d, want %d", tc.u, got, tc.want)
		}
	}
}
