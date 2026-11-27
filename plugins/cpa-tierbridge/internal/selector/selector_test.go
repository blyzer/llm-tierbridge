package selector

import (
	"testing"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/estimate"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
)

var t0 = time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)

func cand(provider, model string, window int, tools bool, input ...string) routerfile.Candidate {
	return routerfile.Candidate{Provider: provider, Model: model, ContextWindow: window, SupportsTools: tools, Input: input}
}

func TestSelect(t *testing.T) {
	ranking := []routerfile.Candidate{
		cand("small", "s", 1000, true, "text", "image"),      // 1
		cand("notools", "n", 100000, false, "text", "image"), // 2
		cand("textonly", "t", 100000, true, "text"),          // 3
		cand("offline", "o", 100000, true, "text", "image"),  // 4
		cand("big", "b", 0, true, "text", "image"),           // 5: unknown window always fits
	}
	tests := []struct {
		name      string
		req       estimate.Request
		available []string
		cands     []routerfile.Candidate
		wantRank  int // 0 = nothing fits
		wantSkips []SkipReason
		wantText  string
	}{
		{
			name:     "first candidate wins",
			req:      estimate.Request{Tokens: 10},
			cands:    ranking,
			wantRank: 1, wantText: "rank 1 of 5",
		},
		{
			name:      "context window skip",
			req:       estimate.Request{Tokens: 5000},
			cands:     ranking,
			wantRank:  2,
			wantSkips: []SkipReason{SkipContext},
			wantText:  "rank 2 of 5 (skipped 1: small/s context)",
		},
		{
			name:      "window boundary fits",
			req:       estimate.Request{Tokens: 1000},
			cands:     ranking,
			wantRank:  1,
			wantSkips: nil,
			wantText:  "rank 1 of 5",
		},
		{
			name:      "tools skip",
			req:       estimate.Request{Tokens: 5000, Tools: true},
			cands:     ranking,
			wantRank:  3,
			wantSkips: []SkipReason{SkipContext, SkipTools},
			wantText:  "rank 3 of 5 (skipped 2: small/s context, notools/n tools)",
		},
		{
			name:      "image skip",
			req:       estimate.Request{Tokens: 5000, Image: true},
			cands:     ranking,
			wantRank:  2,
			wantSkips: []SkipReason{SkipContext},
			wantText:  "rank 2 of 5 (skipped 1: small/s context)",
		},
		{
			name:      "tools and image skip",
			req:       estimate.Request{Tokens: 5000, Tools: true, Image: true},
			cands:     ranking,
			wantRank:  4,
			wantSkips: []SkipReason{SkipContext, SkipTools, SkipImage},
			wantText:  "rank 4 of 5 (skipped 3: small/s context, notools/n tools, textonly/t image)",
		},
		{
			name:      "provider not available skip",
			req:       estimate.Request{Tokens: 5000, Tools: true, Image: true},
			available: []string{"small", "big"},
			cands:     ranking,
			wantRank:  5,
			wantSkips: []SkipReason{SkipContext, SkipTools, SkipImage, SkipProvider},
			wantText:  "rank 5 of 5 (skipped 4: small/s context, notools/n tools, textonly/t image, offline/o provider)",
		},
		{
			name:      "available list is case-insensitive",
			req:       estimate.Request{Tokens: 10},
			available: []string{" SMALL "},
			cands:     ranking,
			wantRank:  1,
			wantText:  "rank 1 of 5",
		},
		{
			name:      "empty available list means no provider filter",
			req:       estimate.Request{Tokens: 10},
			available: nil,
			cands:     ranking[3:4],
			wantRank:  1,
			wantText:  "rank 1 of 1",
		},
		{
			name:      "nothing fits",
			req:       estimate.Request{Tokens: 500000, Tools: true},
			cands:     ranking[:4],
			wantRank:  0,
			wantSkips: []SkipReason{SkipContext, SkipContext, SkipContext, SkipContext},
			wantText:  "no candidate fits (small/s context, notools/n context, textonly/t context, offline/o context)",
		},
		{
			name:     "no candidates",
			req:      estimate.Request{Tokens: 1},
			cands:    nil,
			wantRank: 0,
			wantText: "no candidates",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := Select(tc.req, tc.cands, tc.available, nil, t0)
			if got.Rank != tc.wantRank || (got.Picked != nil) != (tc.wantRank != 0) {
				t.Fatalf("Rank = %d (picked %v), want %d", got.Rank, got.Picked, tc.wantRank)
			}
			if got.Picked != nil && got.Picked.Key() != tc.cands[tc.wantRank-1].Key() {
				t.Errorf("Picked = %s, want %s", got.Picked.Key(), tc.cands[tc.wantRank-1].Key())
			}
			if got.Of != len(tc.cands) {
				t.Errorf("Of = %d, want %d", got.Of, len(tc.cands))
			}
			if len(got.Skips) != len(tc.wantSkips) {
				t.Fatalf("Skips = %+v, want reasons %v", got.Skips, tc.wantSkips)
			}
			for i, s := range got.Skips {
				if s.Reason != tc.wantSkips[i] || s.Rank != i+1 {
					t.Errorf("Skips[%d] = %+v, want rank %d reason %s", i, s, i+1, tc.wantSkips[i])
				}
			}
			if text := got.Reason(); text != tc.wantText {
				t.Errorf("Reason() = %q, want %q", text, tc.wantText)
			}
		})
	}
}

func TestReasonTruncatesLongSkipList(t *testing.T) {
	var cands []routerfile.Candidate
	for i := range 8 {
		cands = append(cands, cand("p", string(rune('a'+i)), 10, true))
	}
	cands = append(cands, cand("p", "z", 0, true))
	got := Select(estimate.Request{Tokens: 100}, cands, nil, nil, t0).Reason()
	want := "rank 9 of 9 (skipped 8: p/a context, p/b context, p/c context, p/d context, p/e context, +3 more)"
	if got != want {
		t.Errorf("Reason() = %q, want %q", got, want)
	}
}

// fakeLimiter allows each candidate RPM requests per sliding minute (0 = unlimited).
type fakeLimiter struct {
	seen map[string][]time.Time
}

func (f *fakeLimiter) Reserve(c routerfile.Candidate, _ estimate.Request, now time.Time) (bool, SkipReason) {
	if c.RPM == 0 {
		return true, ""
	}
	if f.seen == nil {
		f.seen = map[string][]time.Time{}
	}
	var keep []time.Time
	for _, t := range f.seen[c.Key()] {
		if t.After(now.Add(-time.Minute)) {
			keep = append(keep, t)
		}
	}
	if len(keep) >= c.RPM {
		f.seen[c.Key()] = keep
		return false, SkipRPM
	}
	f.seen[c.Key()] = append(keep, now)
	return true, ""
}

func TestSelectWithLimiter(t *testing.T) {
	ranking := []routerfile.Candidate{
		{Provider: "a", Model: "one", RPM: 1},
		{Provider: "b", Model: "two"},
	}
	lim := &fakeLimiter{}

	first := Select(estimate.Request{}, ranking, nil, lim, t0)
	if first.Rank != 1 {
		t.Fatalf("first request: rank %d, want 1", first.Rank)
	}
	second := Select(estimate.Request{}, ranking, nil, lim, t0.Add(time.Second))
	if second.Rank != 2 || len(second.Skips) != 1 || second.Skips[0].Reason != SkipRPM {
		t.Fatalf("rpm 1: second request = rank %d skips %+v, want rank 2 after an rpm skip", second.Rank, second.Skips)
	}
	// A minute later the window has slid past the first request.
	third := Select(estimate.Request{}, ranking, nil, lim, t0.Add(61*time.Second))
	if third.Rank != 1 {
		t.Errorf("after the window: rank %d, want 1", third.Rank)
	}
}

func TestSelectLimiterNotConsultedForStaticSkips(t *testing.T) {
	// Quota is consumed only by the candidate that is actually picked.
	ranking := []routerfile.Candidate{
		{Provider: "a", Model: "one", ContextWindow: 10, RPM: 1},
		{Provider: "b", Model: "two"},
	}
	lim := &fakeLimiter{}
	if got := Select(estimate.Request{Tokens: 50}, ranking, nil, lim, t0); got.Rank != 2 {
		t.Fatalf("rank %d, want 2", got.Rank)
	}
	if got := Select(estimate.Request{Tokens: 5}, ranking, nil, lim, t0); got.Rank != 1 {
		t.Errorf("rank %d, want 1: a context skip must not consume the rpm slot", got.Rank)
	}
}

func TestRecorder(t *testing.T) {
	dec := func(model string) Decision { return Decision{RequestedModel: model} }
	models := func(ds []Decision) []string {
		var out []string
		for _, d := range ds {
			out = append(out, d.RequestedModel)
		}
		return out
	}
	eq := func(got []string, want ...string) bool {
		if len(got) != len(want) {
			return false
		}
		for i := range got {
			if got[i] != want[i] {
				return false
			}
		}
		return true
	}

	r := NewRecorder(3)
	if got := r.Recent(); len(got) != 0 {
		t.Fatalf("empty ring = %v", models(got))
	}
	r.Add(dec("1"))
	r.Add(dec("2"))
	if got := models(r.Recent()); !eq(got, "2", "1") {
		t.Errorf("Recent() = %v, want [2 1]", got)
	}
	r.Add(dec("3"))
	r.Add(dec("4"))
	r.Add(dec("5"))
	if got := models(r.Recent()); !eq(got, "5", "4", "3") {
		t.Errorf("full ring Recent() = %v, want [5 4 3]", got)
	}
	r.SetKeep(2)
	if got := models(r.Recent()); !eq(got, "5", "4") {
		t.Errorf("after shrinking Recent() = %v, want [5 4]", got)
	}
	r.Add(dec("6"))
	if got := models(r.Recent()); !eq(got, "6", "5") {
		t.Errorf("after shrink+add Recent() = %v, want [6 5]", got)
	}
	r.SetKeep(4)
	r.Add(dec("7"))
	if got := models(r.Recent()); !eq(got, "7", "6", "5") {
		t.Errorf("after growing Recent() = %v, want [7 6 5]", got)
	}
	r.SetKeep(0)
	r.Add(dec("8"))
	if got := r.Recent(); len(got) != 0 {
		t.Errorf("keep 0 holds %v", models(got))
	}
	r.SetKeep(2)
	r.Add(dec("9"))
	if got := models(r.Recent()); !eq(got, "9") {
		t.Errorf("after re-enabling Recent() = %v, want [9]", got)
	}
	if got := NewRecorder(-1); got.keep != 0 {
		t.Errorf("negative keep = %d, want 0", got.keep)
	}
}
