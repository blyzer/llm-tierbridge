// Package routerfile loads and validates the ranking file (tierbridge-router.json) the plugin routes by,
// and reloads it when it changes on disk.
//
// # File format
//
// An object with a "candidates" array (a bare array is accepted too), best candidate first: the array
// order IS the rank, the plugin picks the first candidate that fits the request.
//
//	{
//	  "version": 1,
//	  "generatedAt": "2026-11-27T12:00:00Z",
//	  "candidates": [
//	    {"provider": "openai", "model": "gpt-4.1", "contextWindow": 1047576,
//	     "supportsTools": true, "input": ["text", "image"], "rpm": 500, "rpd": null}
//	  ]
//	}
//
// Per candidate: "provider" (the CLIProxyAPI provider key the request is routed through) and "model"
// are required; "contextWindow" in tokens (null/0/absent = unknown, never a reason to skip);
// "supportsTools" (absent/null = false: skipped for requests that declare tools); "input" lists the
// accepted modalities (absent = text only, "image" is required for requests with images); "rpm"
// and "rpd" are request quotas per minute and per UTC day (null/0/absent = unlimited). Unknown keys
// are ignored. "version", when present, must be 1.
package routerfile

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"
)

// SupportedVersion is the only file format version.
const SupportedVersion = 1

// ErrNoPath is returned by Store.Current when no router file is configured.
var ErrNoPath = errors.New("no router-file configured")

// Candidate is one ranked provider/model pair.
type Candidate struct {
	Provider      string   `json:"provider"`
	Model         string   `json:"model"`
	ContextWindow int      `json:"contextWindow"`
	SupportsTools bool     `json:"supportsTools"`
	Input         []string `json:"input"`
	RPM           int      `json:"rpm"`
	RPD           int      `json:"rpd"`
}

// HasInput reports whether the candidate accepts the modality ("text", "image", ...).
func (c Candidate) HasInput(kind string) bool {
	for _, in := range c.Input {
		if in == kind {
			return true
		}
	}
	return false
}

// Key identifies the candidate in quota counters.
func (c Candidate) Key() string {
	return c.Provider + "/" + c.Model
}

// File is a validated router file.
type File struct {
	Version     int         `json:"version"`
	GeneratedAt string      `json:"generatedAt,omitempty"`
	Candidates  []Candidate `json:"candidates"`
}

// Parse decodes and validates a router file, normalising providers (lower case) and modalities.
func Parse(data []byte) (*File, error) {
	data = bytes.TrimSpace(data)
	if len(data) == 0 {
		return nil, errors.New("router file is empty")
	}
	var f File
	if data[0] == '[' {
		if err := json.Unmarshal(data, &f.Candidates); err != nil {
			return nil, fmt.Errorf("router file: %w", err)
		}
	} else if err := json.Unmarshal(data, &f); err != nil {
		return nil, fmt.Errorf("router file: %w", err)
	}
	if f.Version != 0 && f.Version != SupportedVersion {
		return nil, fmt.Errorf("router file: unsupported version %d (want %d)", f.Version, SupportedVersion)
	}
	if len(f.Candidates) == 0 {
		return nil, errors.New("router file: no candidates")
	}
	seen := make(map[string]int, len(f.Candidates))
	for i := range f.Candidates {
		c := &f.Candidates[i]
		c.Provider = strings.ToLower(strings.TrimSpace(c.Provider))
		c.Model = strings.TrimSpace(c.Model)
		if c.Provider == "" || c.Model == "" {
			return nil, fmt.Errorf("router file: candidate %d: provider and model are required", i+1)
		}
		if c.ContextWindow < 0 || c.RPM < 0 || c.RPD < 0 {
			return nil, fmt.Errorf("router file: candidate %d (%s): contextWindow, rpm and rpd must not be negative", i+1, c.Key())
		}
		input := make([]string, 0, len(c.Input))
		for _, in := range c.Input {
			if in = strings.ToLower(strings.TrimSpace(in)); in != "" {
				input = append(input, in)
			}
		}
		c.Input = input
		if first, dup := seen[c.Key()]; dup {
			return nil, fmt.Errorf("router file: candidate %d repeats %s (first at %d)", i+1, c.Key(), first)
		}
		seen[c.Key()] = i + 1
	}
	f.Version = SupportedVersion
	return &f, nil
}

// Status is a snapshot of the store for the management status.
type Status struct {
	Path       string    `json:"path"`
	Candidates int       `json:"candidates"`
	LoadedAt   time.Time `json:"loadedAt,omitzero"`
	// Error is the last load problem (stat, read or validation), empty when the file is healthy. The
	// previous good version keeps serving while it is set.
	Error string `json:"error,omitempty"`
}

type signature struct {
	size    int64
	modTime time.Time
}

// Store serves the current good router file and reloads it when its mtime or size changes. A new
// version that fails validation never replaces the previous good one.
type Store struct {
	path string
	now  func() time.Time

	mu       sync.Mutex
	file     *File
	loadedAt time.Time
	sig      signature // of the last version read, good or not: an unchanged bad file is not re-read
	haveSig  bool
	loadErr  error // validation/read error of the version identified by sig
	statErr  error // the file cannot be stat-ed right now
}

// NewStore creates a store for path; the file is read on the first Current call.
func NewStore(path string) *Store {
	return &Store{path: path, now: time.Now}
}

// Path returns the configured path.
func (s *Store) Path() string { return s.path }

// Current returns the last good file (nil when none was ever loaded) and the latest problem, if any.
// It costs one stat when the file did not change.
func (s *Store) Current() (*File, error) {
	if s.path == "" {
		return nil, ErrNoPath
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.refreshLocked()
	return s.file, s.errLocked()
}

// Status reports the store state without reloading.
func (s *Store) Status() Status {
	s.mu.Lock()
	defer s.mu.Unlock()
	st := Status{Path: s.path, LoadedAt: s.loadedAt}
	if s.file != nil {
		st.Candidates = len(s.file.Candidates)
	}
	if err := s.errLocked(); err != nil {
		st.Error = err.Error()
	}
	return st
}

func (s *Store) errLocked() error {
	if s.statErr != nil {
		return s.statErr
	}
	return s.loadErr
}

func (s *Store) refreshLocked() {
	info, err := os.Stat(s.path)
	if err != nil {
		s.statErr = err
		return
	}
	s.statErr = nil
	sig := signature{size: info.Size(), modTime: info.ModTime()}
	if s.haveSig && sig == s.sig {
		return
	}
	s.sig, s.haveSig = sig, true
	data, err := os.ReadFile(s.path)
	if err != nil {
		s.loadErr = err
		return
	}
	file, err := Parse(data)
	if err != nil {
		s.loadErr = err
		return
	}
	s.file, s.loadErr, s.loadedAt = file, nil, s.now()
}
