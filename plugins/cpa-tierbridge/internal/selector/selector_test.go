package selector

import (
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/estimate"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
)

func cand(model string, window int, tools bool, input ...string) routerfile.Candidate {
	return routerfile.Candidate{Model: model, ContextWindow: window, SupportsTools: tools, Input: input}
}

func TestFit(t *testing.T) {
	tests := []struct {
		name string
		req  estimate.Request
		c    routerfile.Candidate
		want SkipReason
	}{
		{"fits", estimate.Request{Tokens: 100}, cand("a", 1000, true, "text"), ""},
		{"unknown window always fits", estimate.Request{Tokens: 1 << 30}, cand("a", 0, false), ""},
		{"context", estimate.Request{Tokens: 2000}, cand("a", 1000, true), SkipContext},
		{"tools", estimate.Request{Tools: true}, cand("a", 1000, false), SkipTools},
		{"image", estimate.Request{Image: true}, cand("a", 1000, true, "text"), SkipImage},
		{"image accepted", estimate.Request{Image: true}, cand("a", 1000, true, "text", "image"), ""},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := Fit(tc.req, tc.c); got != tc.want {
				t.Errorf("Fit() = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestSelect(t *testing.T) {
	cands := []routerfile.Candidate{
		cand("small", 100, true, "text"),
		cand("notools", 10000, false, "text"),
		cand("textonly", 10000, true, "text"),
		cand("ok", 10000, true, "text", "image"),
	}
	out := Select(estimate.Request{Tokens: 500, Tools: true, Image: true}, cands, nil, time.Time{})
	if out.Picked == nil || out.Picked.Model != "ok" || out.Rank != 4 || out.Of != 4 {
		t.Fatalf("Select() = %+v", out)
	}
	want := "rank 4 of 4 (skipped 3: small context, notools tools, textonly image)"
	if got := out.Reason(); got != want {
		t.Errorf("Reason() = %q, want %q", got, want)
	}

	none := Select(estimate.Request{Tokens: 1 << 20}, cands, nil, time.Time{})
	if none.Picked != nil || !strings.HasPrefix(none.Reason(), "no candidate fits (") {
		t.Errorf("Select() with nothing fitting = %+v / %q", none, none.Reason())
	}
	if empty := Select(estimate.Request{}, nil, nil, time.Time{}); empty.Picked != nil || empty.Reason() != "no candidates" {
		t.Errorf("Select(no candidates) = %+v / %q", empty, empty.Reason())
	}
}

func TestReasonTruncatesLongSkipList(t *testing.T) {
	var cands []routerfile.Candidate
	for i := range 8 {
		cands = append(cands, cand(fmt.Sprintf("m%d", i), 10, true))
	}
	got := Select(estimate.Request{Tokens: 999}, cands, nil, time.Time{}).Reason()
	if !strings.Contains(got, "m4 context, +3 more") {
		t.Errorf("Reason() = %q, want a list cut at 5 with a +3 more tail", got)
	}
}

type fakeLimiter struct {
	refuse map[string]SkipReason
	asked  []string
}

func (f *fakeLimiter) Reserve(c routerfile.Candidate, _ estimate.Request, _ time.Time) (bool, SkipReason) {
	f.asked = append(f.asked, c.Model)
	if why, no := f.refuse[c.Model]; no {
		return false, why
	}
	return true, ""
}

func TestSelectWithLimiter(t *testing.T) {
	lim := &fakeLimiter{refuse: map[string]SkipReason{"a": SkipRPM, "b": SkipCooldown}}
	cands := []routerfile.Candidate{cand("a", 0, true), cand("b", 0, true), cand("c", 0, true), cand("d", 0, true)}
	out := Select(estimate.Request{}, cands, lim, time.Time{})
	if out.Picked == nil || out.Picked.Model != "c" || out.Rank != 3 {
		t.Fatalf("Select() = %+v", out)
	}
	if len(out.Skips) != 2 || out.Skips[0].Reason != SkipRPM || out.Skips[1].Reason != SkipCooldown {
		t.Errorf("skips = %+v", out.Skips)
	}
	if strings.Join(lim.asked, ",") != "a,b,c" {
		t.Errorf("limiter asked %v, want it to stop at the first grant", lim.asked)
	}
}

func TestSelectLimiterNotConsultedForStaticSkips(t *testing.T) {
	lim := &fakeLimiter{}
	Select(estimate.Request{Tools: true}, []routerfile.Candidate{cand("a", 0, false), cand("b", 0, true)}, lim, time.Time{})
	if strings.Join(lim.asked, ",") != "b" {
		t.Errorf("limiter asked %v, want only b", lim.asked)
	}
}

func TestRecorder(t *testing.T) {
	r := NewRecorder(3)
	for i := 1; i <= 5; i++ {
		r.Add(Decision{Rank: i, Attempts: []Attempt{{Rank: i, Model: "m", Status: 200, OK: true}}})
	}
	got := r.Recent()
	if len(got) != 3 || got[0].Rank != 5 || got[2].Rank != 3 || len(got[0].Attempts) != 1 {
		t.Fatalf("Recent() = %+v, want the last 3 newest first, attempts kept", got)
	}
	r.SetKeep(1)
	if got := r.Recent(); len(got) != 1 || got[0].Rank != 5 {
		t.Errorf("after SetKeep(1) = %+v", got)
	}
	r.SetKeep(0)
	r.Add(Decision{Rank: 9})
	if got := r.Recent(); len(got) != 0 {
		t.Errorf("keep 0 must hold nothing, got %+v", got)
	}
}
