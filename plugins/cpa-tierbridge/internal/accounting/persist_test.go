package accounting

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/routerfile"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/selector"
)

// fakeClock is a mutable clock plus a manual replacement for time.AfterFunc.
type fakeClock struct {
	now     time.Time
	pending map[int]func() // scheduled and not cancelled
	next    int
	delays  []time.Duration
}

func (c *fakeClock) Now() time.Time { return c.now }

func (c *fakeClock) After(d time.Duration, f func()) func() {
	if c.pending == nil {
		c.pending = map[int]func(){}
	}
	id := c.next
	c.next++
	c.pending[id] = f
	c.delays = append(c.delays, d)
	return func() { delete(c.pending, id) }
}

// fire runs the scheduled writes that were not cancelled.
func (c *fakeClock) fire() {
	p := c.pending
	c.pending = map[int]func(){}
	for _, f := range p {
		f()
	}
}

func newPersisted(t *testing.T, path string, clk *fakeClock) (*Accountant, *Persister) {
	t.Helper()
	a := New()
	p := NewPersister(a, clk.Now)
	p.after = clk.After
	a.SetOnChange(p.Notify)
	p.SetPath(path)
	return a, p
}

func TestPersistRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state", "tierbridge-state.json")
	clk := &fakeClock{now: t0}
	a, p := newPersisted(t, path, clk)
	a.Configure(10, time.Minute)

	c := paid("m")
	c.RPM, c.RPD, c.TPM, c.TPD = 5, 50, 10_000, 100_000
	mustReserve(t, a, c, 100, t0)
	mustReserve(t, a, c, 100, t0.Add(time.Second))
	a.Charge(c, Usage{InputTokens: 1_000_000, OutputTokens: 500_000, TotalTokens: 1_500_000}, t0.Add(2*time.Second))
	a.Charge(c, Usage{Failed: true, StatusCode: 429}, t0.Add(3*time.Second))
	u := unknown("u")
	a.Charge(u, Usage{TotalTokens: 7}, t0.Add(3*time.Second))
	p.Close()
	if st := p.Status(); st.Saves != 1 || st.SaveError != "" || st.Dirty {
		t.Fatalf("status after Close = %+v", st)
	}
	if info, err := os.Stat(path); err != nil || (runtime.GOOS != "windows" && info.Mode().Perm() != 0o600) {
		t.Fatalf("state file: %v mode %v", err, info)
	}
	if left, _ := filepath.Glob(path + ".tmp-*"); len(left) != 0 {
		t.Errorf("temporary files left behind: %v", left)
	}

	// A new process, ten seconds later, reads the same counters back.
	clk2 := &fakeClock{now: t0.Add(10 * time.Second)}
	a2, p2 := newPersisted(t, path, clk2)
	if st := p2.Status(); !st.Loaded || st.LoadError != "" {
		t.Fatalf("load status = %+v", st)
	}
	got := a2.Counters([]routerfile.Candidate{c}, clk2.now)[0]
	if got.RPM.Used != 2 || got.RPD.Used != 2 || got.TPM.Used != 1_500_000 || got.TPD.Used != 1_500_000 {
		t.Errorf("counters = %+v", got)
	}
	if got.CooldownRemainingSeconds != 53 { // 60 s from t0+3 s, seen at t0+10 s
		t.Errorf("cooldown remaining = %d, want 53", got.CooldownRemainingSeconds)
	}
	b := a2.Budget(clk2.now, nil)
	if b.SpentUSD < 1.999999 || b.SpentUSD > 2.000001 || len(b.Unpriced) != 1 || b.Unpriced[0] != "u" {
		t.Errorf("budget = %+v", b)
	}
	mustRefuse(t, a2, c, 0, clk2.now, selector.SkipCooldown)
}

func TestImportDropsExpiredWindows(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	clk := &fakeClock{now: t0}
	a, p := newPersisted(t, path, clk)
	a.Configure(0, 30*time.Second)
	c := paid("m")
	mustReserve(t, a, c, 0, t0)
	a.Charge(c, Usage{TotalTokens: 100, InputTokens: 1_000_000, Failed: true, StatusCode: 500}, t0)
	p.Close()

	t.Run("same day, windows expired", func(t *testing.T) {
		now := t0.Add(5 * time.Minute)
		a2, _ := newPersisted(t, path, &fakeClock{now: now})
		got := a2.Counters([]routerfile.Candidate{c}, now)[0]
		if got.RPM.Used != 0 || got.TPM.Used != 0 || got.CooldownRemainingSeconds != 0 {
			t.Errorf("minute windows survived: %+v", got)
		}
		if got.RPD.Used != 1 || got.TPD.Used != 100 || a2.Spent(now) == 0 {
			t.Errorf("daily counters lost: %+v spent %v", got, a2.Spent(now))
		}
	})
	t.Run("next day, daily counters expired", func(t *testing.T) {
		now := time.Date(2026, 11, 28, 0, 0, 5, 0, time.UTC)
		a2, _ := newPersisted(t, path, &fakeClock{now: now})
		got := a2.Counters([]routerfile.Candidate{c}, now)[0]
		if got.RPD.Used != 0 || got.TPD.Used != 0 || a2.Spent(now) != 0 {
			t.Errorf("daily counters survived the day: %+v spent %v", got, a2.Spent(now))
		}
	})
}

func TestCorruptedStateStartsEmpty(t *testing.T) {
	for name, content := range map[string]string{
		"garbage":       "{not json",
		"truncated":     `{"version":1,"day":"2026-11-27","models":{"m":{"rpd":`,
		"wrong version": `{"version":99,"models":{}}`,
		"negative":      `{"version":1,"day":"2026-11-27","models":{"m":{"rpd":-4}}}`,
		"binary":        "\x00\x01\x02",
	} {
		t.Run(name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "state.json")
			if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
				t.Fatal(err)
			}
			clk := &fakeClock{now: t0}
			a, p := newPersisted(t, path, clk)
			st := p.Status()
			if st.Loaded || st.LoadError == "" || !st.Enabled {
				t.Errorf("status = %+v, want a load error", st)
			}
			c := free("m")
			c.RPD = 1
			mustReserve(t, a, c, 0, t0) // empty counters
			p.Close()                   // the next save replaces the corrupted file
			clk2 := &fakeClock{now: t0.Add(time.Second)}
			_, p2 := newPersisted(t, path, clk2)
			if st := p2.Status(); !st.Loaded || st.LoadError != "" {
				t.Errorf("after the next save: %+v", st)
			}
		})
	}
}

func TestMissingStateFileIsNotAnError(t *testing.T) {
	_, p := newPersisted(t, filepath.Join(t.TempDir(), "nope.json"), &fakeClock{now: t0})
	if st := p.Status(); st.Loaded || st.LoadError != "" || !st.Enabled {
		t.Errorf("status = %+v", st)
	}
}

func TestEmptyPathMeansNoPersistence(t *testing.T) {
	clk := &fakeClock{now: t0}
	a, p := newPersisted(t, "", clk)
	mustReserve(t, a, free("m"), 0, t0)
	p.Close()
	if len(clk.pending) != 0 {
		t.Errorf("a write was scheduled without a path")
	}
	if st := p.Status(); st.Enabled || st.Saves != 0 || st.Dirty {
		t.Errorf("status = %+v", st)
	}
}

func TestWritesAreThrottledToOnePerInterval(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	clk := &fakeClock{now: t0}
	a, p := newPersisted(t, path, clk)
	c := free("m")

	mustReserve(t, a, c, 0, t0) // the first change is written without delay
	if len(clk.delays) != 1 || clk.delays[0] != 0 {
		t.Fatalf("delays = %v, want one immediate write", clk.delays)
	}
	clk.fire()
	if st := p.Status(); st.Saves != 1 {
		t.Fatalf("saves = %d", st.Saves)
	}

	// Changes inside the interval share one scheduled write, SaveInterval after the previous one.
	clk.now = t0.Add(time.Second)
	mustReserve(t, a, c, 0, clk.now)
	mustReserve(t, a, c, 0, clk.now)
	a.Charge(c, Usage{TotalTokens: 5}, clk.now)
	if len(clk.pending) != 1 || clk.delays[1] != SaveInterval-time.Second {
		t.Fatalf("pending %d delays %v, want one write in %v", len(clk.pending), clk.delays, SaveInterval-time.Second)
	}
	if st := p.Status(); !st.Dirty || st.Saves != 1 {
		t.Fatalf("status = %+v, want dirty and one save", st)
	}
	clk.now = t0.Add(SaveInterval)
	clk.fire()
	if st := p.Status(); st.Dirty || st.Saves != 2 {
		t.Fatalf("status = %+v, want clean and two saves", st)
	}
	data, err := os.ReadFile(path)
	if err != nil || !strings.Contains(string(data), `"rpd":3`) {
		t.Errorf("state file = %s (%v)", data, err)
	}

	// Close writes what is pending even inside the interval.
	clk.now = t0.Add(SaveInterval + time.Second)
	mustReserve(t, a, c, 0, clk.now)
	p.Close()
	if st := p.Status(); st.Dirty || st.Saves != 3 {
		t.Errorf("status after Close = %+v", st)
	}
	clk.fire() // the cancelled timer does nothing
	if st := p.Status(); st.Saves != 3 {
		t.Errorf("saves = %d after the cancelled timer fired", st.Saves)
	}
}

func TestSaveFailureIsReportedAndRetried(t *testing.T) {
	dir := t.TempDir()
	blocker := filepath.Join(dir, "file")
	if err := os.WriteFile(blocker, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	clk := &fakeClock{now: t0}
	a, p := newPersisted(t, filepath.Join(blocker, "state.json"), clk) // its parent is a regular file
	mustReserve(t, a, free("m"), 0, t0)
	p.Close()
	if st := p.Status(); st.SaveError == "" || !st.Dirty || st.Saves != 0 {
		t.Errorf("status = %+v, want a save error and still dirty", st)
	}
}

func TestChangingThePathLoadsTheNewFile(t *testing.T) {
	dir := t.TempDir()
	one, two := filepath.Join(dir, "one.json"), filepath.Join(dir, "two.json")
	clk := &fakeClock{now: t0}
	a, p := newPersisted(t, one, clk)
	c := free("m")
	c.RPD = 10
	mustReserve(t, a, c, 0, t0)
	p.SetPath(two) // flushes the pending change to one.json first
	if _, err := os.Stat(one); err != nil {
		t.Fatalf("pending state was not flushed to the old path: %v", err)
	}
	p.SetPath(one)
	if got := a.Counters([]routerfile.Candidate{c}, t0)[0]; got.RPD.Used != 1 {
		t.Errorf("counters after reloading one.json = %+v", got)
	}
}
