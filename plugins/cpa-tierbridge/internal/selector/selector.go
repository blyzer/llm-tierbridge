// Package selector picks the first ranked candidate that fits a request and remembers what it decided.
package selector

import (
	"fmt"
	"strings"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/estimate"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
)

// SkipReason is a short machine-readable reason a candidate was passed over.
type SkipReason string

// Reasons a candidate does not fit.
const (
	SkipContext  SkipReason = "context"  // the estimated prompt exceeds the context window
	SkipTools    SkipReason = "tools"    // the request declares tools and the model does not support them
	SkipImage    SkipReason = "image"    // the request carries an image and the model does not take image input
	SkipAlias    SkipReason = "alias"    // the candidate is an alias name: the plugin never executes an alias
	SkipRPM      SkipReason = "rpm"      // the per-minute request quota is used up
	SkipRPD      SkipReason = "rpd"      // the per-day request quota is used up
	SkipTPM      SkipReason = "tpm"      // the request does not fit the per-minute token quota
	SkipTPD      SkipReason = "tpd"      // the request does not fit the per-day token quota
	SkipBudget   SkipReason = "budget"   // the request would exceed the daily budget
	SkipCooldown SkipReason = "cooldown" // the candidate failed upstream recently and is cooling down
)

// Limiter decides whether a candidate may take one more request right now.
//
// Reserve is called only for candidates that already fit the request statically, in rank order, and
// it must be atomic: when it returns true it has consumed one request from the candidate's quota, so
// the caller stops at the first true. A false result must leave every counter untouched. req carries
// the estimated prompt size, which token quotas and the budget are checked against.
type Limiter interface {
	Reserve(c routerfile.Candidate, req estimate.Request, now time.Time) (ok bool, why SkipReason)
}

// Skip records why one candidate was passed over.
type Skip struct {
	Rank   int        `json:"rank"`
	Model  string     `json:"model"`
	Reason SkipReason `json:"reason"`
}

// Outcome is the result of one selection.
type Outcome struct {
	// Picked is nil when no candidate fits.
	Picked *routerfile.Candidate
	// Rank is the 1-based position of Picked in the file; Of is the number of candidates.
	Rank, Of int
	// Skips lists the candidates passed over before the pick (or all of them when nothing fits).
	Skips []Skip
}

// Fit reports why c cannot take req on static grounds, or "" when it fits: the estimated prompt must
// not exceed its context window (unknown window = fits), it must support tools if the request declares
// any, and accept images if the request has any. Quotas, budget and cooldown are the Limiter's business.
func Fit(req estimate.Request, c routerfile.Candidate) SkipReason {
	switch {
	case c.ContextWindow > 0 && req.Tokens > c.ContextWindow:
		return SkipContext
	case req.Tools && !c.SupportsTools:
		return SkipTools
	case req.Image && !c.HasInput("image"):
		return SkipImage
	}
	return ""
}

// Select returns the first candidate of cands that fits req (see Fit) and that lim allows one more
// request on. lim may be nil (no quotas: a preview that consumes nothing).
func Select(req estimate.Request, cands []routerfile.Candidate, lim Limiter, now time.Time) Outcome {
	out := Outcome{Of: len(cands)}
	for i := range cands {
		c := cands[i]
		if why := Fit(req, c); why != "" {
			out.Skips = append(out.Skips, Skip{Rank: i + 1, Model: c.Model, Reason: why})
			continue
		}
		if lim != nil {
			if ok, why := lim.Reserve(c, req, now); !ok {
				out.Skips = append(out.Skips, Skip{Rank: i + 1, Model: c.Model, Reason: why})
				continue
			}
		}
		out.Picked, out.Rank = &cands[i], i+1
		return out
	}
	return out
}

// Reason renders the outcome for ModelRouteResponse.Reason: "rank 2 of 5" plus, when candidates were
// passed over, a compact "(skipped 1: openai/gpt-4.1 context)" tail. Without a pick it reads
// "no candidate fits (<why each was skipped>)".
func (o Outcome) Reason() string {
	var b strings.Builder
	switch {
	case o.Picked != nil:
		fmt.Fprintf(&b, "rank %d of %d", o.Rank, o.Of)
		if len(o.Skips) > 0 {
			fmt.Fprintf(&b, " (skipped %d: %s)", len(o.Skips), o.skipList())
		}
	case len(o.Skips) > 0:
		fmt.Fprintf(&b, "no candidate fits (%s)", o.skipList())
	default:
		b.WriteString("no candidates")
	}
	return b.String()
}

// skipList renders the first few skips as "model reason, ..." with a "+N more" tail.
func (o Outcome) skipList() string {
	const maxListed = 5
	var b strings.Builder
	for i, s := range o.Skips {
		if i == maxListed {
			fmt.Fprintf(&b, ", +%d more", len(o.Skips)-maxListed)
			break
		}
		if i > 0 {
			b.WriteString(", ")
		}
		fmt.Fprintf(&b, "%s %s", s.Model, s.Reason)
	}
	return b.String()
}
