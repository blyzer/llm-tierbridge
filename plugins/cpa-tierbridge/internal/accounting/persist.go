package accounting

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"math"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"time"
)

// StateVersion is the version of the persisted state file.
const StateVersion = 1

// maxImportedCooldown bounds a cooldown read back from the state file.
const maxImportedCooldown = 30 * 24 * time.Hour

func minTime(a, b time.Time) time.Time {
	if a.Before(b) {
		return a
	}
	return b
}

// SaveInterval is the minimum time between two writes of the state file.
const SaveInterval = 5 * time.Second

// State is the persisted form of the counters (decisions and the usage ring are not persisted).
type State struct {
	Version  int                   `json:"version"`
	SavedAt  time.Time             `json:"savedAt"`
	Day      string                `json:"day"`
	SpentUSD float64               `json:"spentUsd"`
	Unpriced []string              `json:"unpriced,omitempty"`
	Models   map[string]ModelState `json:"models"`
}

// ModelState is the persisted counters of one model.
type ModelState struct {
	Requests      []time.Time  `json:"requests,omitempty"`
	RPD           int          `json:"rpd"`
	Tokens        []TokenEvent `json:"tokens,omitempty"`
	TPD           int64        `json:"tpd"`
	CooldownUntil time.Time    `json:"cooldownUntil,omitzero"`
}

// TokenEvent is one token charge inside the tpm window.
type TokenEvent struct {
	At time.Time `json:"at"`
	N  int64     `json:"n"`
}

// Export returns the counters as of now with the expired windows already dropped.
func (a *Accountant) Export(now time.Time) State {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.rollLocked(now)
	st := State{Version: StateVersion, SavedAt: now.UTC(), Day: a.day, SpentUSD: a.spent, Models: map[string]ModelState{}}
	for m := range a.unpriced {
		st.Unpriced = append(st.Unpriced, m)
	}
	sort.Strings(st.Unpriced)
	for name, m := range a.models {
		m.pruneLocked(now)
		ms := ModelState{
			Requests:      append([]time.Time(nil), m.requests...),
			RPD:           m.rpd,
			TPD:           m.tpd,
			CooldownUntil: m.cooldownUntil,
		}
		for _, e := range m.tokens {
			ms.Tokens = append(ms.Tokens, TokenEvent{At: e.at, N: e.n})
		}
		if ms.RPD == 0 && ms.TPD == 0 && len(ms.Requests) == 0 && len(ms.Tokens) == 0 && ms.CooldownUntil.IsZero() {
			continue
		}
		st.Models[name] = ms
	}
	return st
}

// Import replaces the counters with st as of now. Windows that have expired are dropped: the minute
// windows keep only events inside the last Window, the daily counters and the spend survive only when
// st is from the same UTC day, and a cooldown only while it still lasts. A state that is not valid
// (wrong version, negative numbers) is rejected and leaves the counters untouched.
func (a *Accountant) Import(st State, now time.Time) error {
	if st.Version != StateVersion {
		return fmt.Errorf("unsupported state version %d (want %d)", st.Version, StateVersion)
	}
	if st.SpentUSD < 0 || math.IsNaN(st.SpentUSD) || math.IsInf(st.SpentUSD, 0) {
		return errors.New("invalid spentUsd")
	}
	sameDay := st.Day == dayOf(now)
	models := make(map[string]*modelState, len(st.Models))
	cutoff := now.Add(-Window)
	for name, ms := range st.Models {
		if ms.RPD < 0 || ms.TPD < 0 {
			return fmt.Errorf("model %q: negative daily counter", name)
		}
		m := &modelState{}
		for _, t := range ms.Requests {
			if t.After(cutoff) && !t.After(now) {
				m.requests = append(m.requests, t)
			}
		}
		for _, e := range ms.Tokens {
			if e.N < 0 {
				return fmt.Errorf("model %q: negative token event", name)
			}
			if e.At.After(cutoff) && !e.At.After(now) {
				m.tokens = append(m.tokens, tokenEvent{at: e.At, n: clamp(e.N)})
			}
		}
		sort.Slice(m.requests, func(i, j int) bool { return m.requests[i].Before(m.requests[j]) })
		sort.Slice(m.tokens, func(i, j int) bool { return m.tokens[i].at.Before(m.tokens[j].at) })
		if sameDay {
			m.rpd, m.tpd = ms.RPD, clamp(ms.TPD)
		}
		if ms.CooldownUntil.After(now) {
			// A tampered file cannot park a model for longer than any real cooldown.
			m.cooldownUntil = minTime(ms.CooldownUntil, now.Add(maxImportedCooldown))
		}
		models[name] = m
	}
	a.mu.Lock()
	a.models, a.unpriced = models, map[string]bool{}
	a.day, a.spent = dayOf(now), 0
	if sameDay {
		a.spent = st.SpentUSD
		for _, m := range st.Unpriced {
			a.unpriced[m] = true
		}
	}
	a.mu.Unlock()
	return nil
}

// Reset empties every counter.
func (a *Accountant) Reset() {
	a.mu.Lock()
	a.models, a.unpriced, a.day, a.spent = map[string]*modelState{}, map[string]bool{}, "", 0
	a.mu.Unlock()
}

// PersistStatus is the persistence view for the management status.
type PersistStatus struct {
	Enabled bool   `json:"enabled"`
	Path    string `json:"path,omitempty"`
	// Loaded is true when counters were restored from the file at the last (re)load.
	Loaded   bool      `json:"loaded"`
	LoadedAt time.Time `json:"loadedAt,omitzero"`
	// LoadError is set when the file could not be used (for example corrupted): the plugin started empty.
	LoadError  string    `json:"loadError,omitempty"`
	Dirty      bool      `json:"dirty"`
	Saves      int       `json:"saves"`
	LastSaveAt time.Time `json:"lastSaveAt,omitzero"`
	SaveError  string    `json:"saveError,omitempty"`
}

// Persister writes the accountant's counters to a JSON file: atomically (temporary file plus rename,
// mode 0600), at most once per SaveInterval after a change, and on Close. An empty path disables it.
type Persister struct {
	acct *Accountant
	now  func() time.Time
	// after schedules f to run once after d; time.AfterFunc outside tests.
	after func(d time.Duration, f func()) func()

	mu       sync.Mutex
	path     string
	dirty    bool
	stop     func() // cancels the scheduled flush; nil when none is pending
	lastSave time.Time
	status   PersistStatus
}

// NewPersister creates a persister for a. Counters changes are reported to it with a.SetOnChange(p.Notify).
func NewPersister(a *Accountant, now func() time.Time) *Persister {
	return &Persister{
		acct: a,
		now:  now,
		after: func(d time.Duration, f func()) func() {
			t := time.AfterFunc(d, f)
			return func() { t.Stop() }
		},
	}
}

// SetPath points the persister at path (empty disables persistence) and loads the file when the path
// changed. Pending changes are written to the previous path first. A missing file is not an error; a
// file that cannot be read or decoded leaves the counters empty and is reported in the status.
func (p *Persister) SetPath(path string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if path == p.path {
		return
	}
	p.flushLocked()
	p.cancelLocked()
	p.path, p.dirty = path, false
	p.status = PersistStatus{Enabled: path != "", Path: path}
	if path == "" {
		return
	}
	now := p.now()
	p.status.LoadedAt = now
	data, err := os.ReadFile(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return
	case err != nil:
		p.acct.Reset()
		p.status.LoadError = "read state: " + err.Error()
		return
	}
	var st State
	if err := json.Unmarshal(data, &st); err != nil {
		p.acct.Reset()
		p.status.LoadError = "state file is corrupted, starting empty: " + err.Error()
		return
	}
	if err := p.acct.Import(st, now); err != nil {
		p.acct.Reset()
		p.status.LoadError = "state file is invalid, starting empty: " + err.Error()
		return
	}
	p.status.Loaded = true
}

// Notify records that the counters changed and schedules a write.
func (p *Persister) Notify() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.path == "" {
		return
	}
	p.dirty = true
	if p.stop != nil {
		return
	}
	delay := max(p.lastSave.Add(SaveInterval).Sub(p.now()), 0)
	p.stop = p.after(delay, p.Flush)
}

// Flush writes the counters now when something changed since the last write.
func (p *Persister) Flush() {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.cancelLocked()
	p.flushLocked()
}

// Close writes pending changes and stops the scheduled write; the plugin.shutdown hook.
func (p *Persister) Close() { p.Flush() }

// Status returns the persistence view.
func (p *Persister) Status() PersistStatus {
	p.mu.Lock()
	defer p.mu.Unlock()
	st := p.status
	st.Dirty = p.dirty
	return st
}

func (p *Persister) cancelLocked() {
	if p.stop != nil {
		p.stop()
		p.stop = nil
	}
}

func (p *Persister) flushLocked() {
	if p.path == "" || !p.dirty {
		return
	}
	now := p.now()
	p.dirty = false
	if err := writeAtomic(p.path, p.acct.Export(now)); err != nil {
		p.dirty = true
		p.status.SaveError = err.Error()
		return
	}
	p.lastSave = now
	p.status.Saves++
	p.status.LastSaveAt = now
	p.status.SaveError = ""
}

// writeAtomic writes st next to path and renames it over path, so a reader (or a crash) never sees a
// half-written file.
func writeAtomic(path string, st State) error {
	data, err := json.Marshal(st)
	if err != nil {
		return err
	}
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(dir, filepath.Base(path)+".tmp-*")
	if err != nil {
		return err
	}
	name := tmp.Name()
	cleanup := func(err error) error {
		tmp.Close()
		os.Remove(name)
		return err
	}
	if err := tmp.Chmod(0o600); err != nil {
		return cleanup(err)
	}
	if _, err := tmp.Write(data); err != nil {
		return cleanup(err)
	}
	if err := tmp.Sync(); err != nil {
		return cleanup(err)
	}
	if err := tmp.Close(); err != nil {
		os.Remove(name)
		return err
	}
	if err := os.Rename(name, path); err != nil {
		os.Remove(name)
		return err
	}
	return nil
}
