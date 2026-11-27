package selector

import (
	"sync"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
)

// MemoryLimiter is the in-memory Limiter: a sliding one-minute window for rpm, a UTC-day counter for
// rpd, and an optional daily spend budget. Nothing survives a restart.
type MemoryLimiter struct {
	mu       sync.Mutex
	budget   float64 // USD per UTC day; 0 = no budget
	day      string  // UTC day the spend belongs to
	spentUSD float64
	counters map[string]*counter
}

type counter struct {
	recent   []time.Time // request times within the last minute, oldest first
	day      string
	dayCount int
}

// NewMemoryLimiter creates a limiter with the given daily budget in USD (0 = no budget).
func NewMemoryLimiter(dailyBudgetUSD float64) *MemoryLimiter {
	return &MemoryLimiter{budget: dailyBudgetUSD, counters: make(map[string]*counter)}
}

// SetDailyBudget changes the daily budget in USD (0 = no budget).
func (l *MemoryLimiter) SetDailyBudget(usd float64) {
	l.mu.Lock()
	l.budget = usd
	l.mu.Unlock()
}

// AddSpend adds usd to the spend of the UTC day containing now.
func (l *MemoryLimiter) AddSpend(usd float64, now time.Time) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.rollDayLocked(now)
	l.spentUSD += usd
}

// Spent returns the spend of the UTC day containing now.
func (l *MemoryLimiter) Spent(now time.Time) float64 {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.rollDayLocked(now)
	return l.spentUSD
}

// Reserve implements Limiter. The daily budget is checked first (it gates every candidate), then the
// candidate's rpm and rpd; counters are only consumed when all of them pass.
func (l *MemoryLimiter) Reserve(c routerfile.Candidate, now time.Time) (bool, SkipReason) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.rollDayLocked(now)
	if l.budget > 0 && l.spentUSD >= l.budget {
		return false, SkipBudget
	}
	if c.RPM == 0 && c.RPD == 0 {
		return true, ""
	}
	key := c.Key()
	ct := l.counters[key]
	if ct == nil {
		ct = &counter{}
		l.counters[key] = ct
	}
	day := dayOf(now)
	if ct.day != day {
		ct.day, ct.dayCount = day, 0
	}
	if c.RPM > 0 {
		cutoff := now.Add(-time.Minute)
		keep := ct.recent[:0]
		for _, t := range ct.recent {
			if t.After(cutoff) {
				keep = append(keep, t)
			}
		}
		ct.recent = keep
		if len(ct.recent) >= c.RPM {
			return false, SkipRPM
		}
	}
	if c.RPD > 0 && ct.dayCount >= c.RPD {
		return false, SkipRPD
	}
	if c.RPM > 0 {
		ct.recent = append(ct.recent, now)
	}
	if c.RPD > 0 {
		ct.dayCount++
	}
	return true, ""
}

func (l *MemoryLimiter) rollDayLocked(now time.Time) {
	if day := dayOf(now); day != l.day {
		l.day, l.spentUSD = day, 0
	}
}

func dayOf(t time.Time) string {
	return t.UTC().Format("2006-01-02")
}
