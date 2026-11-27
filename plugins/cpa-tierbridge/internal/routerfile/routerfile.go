// Package routerfile loads and validates the ranking file (tierbridge-router.json) the plugin routes by,
// and reloads it when it changes on disk.
//
// # File format (schema "tierbridge-router/2", written by `tierbridge export cpa-router`)
//
//	{
//	  "schema": "tierbridge-router/2", "target": "host", "modelPrefix": "",
//	  "catalogSha256": "<hex>", "quotaSourceSha256": "<hex>|null", "gateway": "omp-live:gw",
//	  "aliases": [
//	    {"alias": "tierbridge/auto", "profile": "free-first", "skipped": {...},
//	     "candidates": [
//	       {"model": "llama-70b", "id": "llama-70b", "key": "omp-live:gw:gw/llama-70b",
//	        "contextWindow": 131072, "maxTokens": 8192, "supportsTools": true, "input": ["text"],
//	        "cost": {"status": "free", "input": 0.0, "output": 0.0}, "confidence": "declared",
//	        "quota": {"rpm": 30, "rpd": 14400, "tpm": 6000, "tpd": 500000,
//	                  "monthlyTokenBudget": "~30M", "source": "...", "matchedBy": "...", "matches": 1},
//	        "reasons": ["known-free"]}
//	     ]}
//	  ]
//	}
//
// /2 is the host mode: "target" is always "host" (the CPA that runs the plugin resolves every
// candidate.model itself, through its own model routing and credential rotation) and candidates carry NO
// provider. "tierbridge-router/1" named a provider per candidate; it is rejected with a clear error.
// Unknown keys are ignored at every level. The profile is per alias.
// The candidates of an alias are best first: the array order IS the rank, and the executor tries the
// first one that fits the request, then falls back down the list. An alias may have no candidates
// (valid: every route then falls through).
//
// Candidate fields: "model" (the name the host accepts, "modelPrefix" + "id") is required and must
// not be an alias name (the plugin never executes its own alias). "contextWindow" in tokens (null/0 =
// unknown, never a reason to skip); "supportsTools" (null = false: skipped for requests that declare
// tools); "input" lists the accepted modalities (null/absent = text only: "image" is required for
// requests with images); "quota.rpm"/"quota.rpd" are request quotas per minute and per UTC day and
// "quota.tpm"/"quota.tpd" the token quotas (null/0 = unlimited; "monthlyTokenBudget" is free text and
// ignored); "cost.status" is free, known or unknown, and a candidate with unknown (or null) prices is
// "unpriced": budget math counts it as 0 and the management status flags it.
package routerfile

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"
)

// Schema is the only supported file schema.
const Schema = "tierbridge-router/2"

// SchemaV1 is the retired schema (a provider per candidate); files carrying it are rejected.
const SchemaV1 = "tierbridge-router/1"

// TargetHost is the only supported "target": the host CPA resolves every candidate.model itself.
const TargetHost = "host"

// ErrNoPath is returned by Store.Current when no router file is configured.
var ErrNoPath = errors.New("no router-file configured")

// Quota is the candidate's rate limits as published by the quota source; nil means unlimited.
type Quota struct {
	RPM *int `json:"rpm"`
	RPD *int `json:"rpd"`
	TPM *int `json:"tpm"`
	TPD *int `json:"tpd"`
	// Source, MatchedBy and Matches say where the limits come from (informational).
	Source    string `json:"source,omitempty"`
	MatchedBy string `json:"matchedBy,omitempty"`
	Matches   int    `json:"matches,omitempty"`
}

// Cost is the candidate's price status. Input/Output are nil when the price is unknown.
type Cost struct {
	Status string   `json:"status"`
	Input  *float64 `json:"input"`
	Output *float64 `json:"output"`
}

// Candidate is one ranked provider/model pair.
type Candidate struct {
	// Model is the model name the host CPA accepts (modelPrefix + id); the executor hands it to
	// host.model.execute, which resolves the provider and credentials itself.
	Model string `json:"model"`
	// ID is the upstream model id and CatalogKey the catalog's stable key (informational, for status).
	ID         string `json:"id,omitempty"`
	CatalogKey string `json:"key,omitempty"`

	ContextWindow int      `json:"contextWindow"`
	MaxTokens     int      `json:"maxTokens"`
	SupportsTools bool     `json:"supportsTools"`
	Input         []string `json:"input"`
	Cost          Cost     `json:"cost"`
	Confidence    string   `json:"confidence,omitempty"`
	Quota         *Quota   `json:"quota"`
	Reasons       []string `json:"reasons,omitempty"`

	// RPM, RPD, TPM and TPD are Quota.rpm, rpd, tpm and tpd flattened by Parse (0 = unlimited).
	RPM int `json:"-"`
	RPD int `json:"-"`
	TPM int `json:"-"`
	TPD int `json:"-"`
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
	return c.Model
}

// Unpriced reports that the candidate has no usable price: budget math counts it as 0 and the
// management status flags it. Free candidates are priced (at zero).
func (c Candidate) Unpriced() bool {
	switch strings.ToLower(c.Cost.Status) {
	case "free":
		return false
	case "known":
		return c.Cost.Input == nil || c.Cost.Output == nil
	default:
		return true
	}
}

// AliasEntry is the ranking of one alias under one profile.
type AliasEntry struct {
	Alias   string `json:"alias"`
	Profile string `json:"profile"`
	// Skipped is what the exporter left out and why; kept verbatim for the management status.
	Skipped    json.RawMessage `json:"skipped,omitempty"`
	Candidates []Candidate     `json:"candidates"`
}

// File is a validated router file.
type File struct {
	Schema            string       `json:"schema"`
	Target            string       `json:"target"`
	ModelPrefix       string       `json:"modelPrefix"`
	CatalogSHA256     string       `json:"catalogSha256"`
	QuotaSourceSHA256 string       `json:"quotaSourceSha256"`
	Gateway           string       `json:"gateway"`
	Aliases           []AliasEntry `json:"aliases"`
}

// Alias returns the entry for name (case-insensitive) or nil.
func (f *File) Alias(name string) *AliasEntry {
	if f == nil {
		return nil
	}
	name = strings.TrimSpace(name)
	for i := range f.Aliases {
		if strings.EqualFold(f.Aliases[i].Alias, name) {
			return &f.Aliases[i]
		}
	}
	return nil
}

// Parse decodes and validates a router file, normalising modalities.
func Parse(data []byte) (*File, error) {
	data = bytes.TrimSpace(data)
	if len(data) == 0 {
		return nil, errors.New("router file is empty")
	}
	var f File
	if err := json.Unmarshal(data, &f); err != nil {
		return nil, fmt.Errorf("router file: %w", err)
	}
	if f.Schema == SchemaV1 {
		return nil, fmt.Errorf("router file: schema %q is no longer supported: it names a provider per candidate and the plugin now runs the candidates through the host; re-export the file with a tierbridge that writes %q (tierbridge export cpa-router)", SchemaV1, Schema)
	}
	if f.Schema != Schema {
		return nil, fmt.Errorf("router file: unsupported schema %q (want %q)", f.Schema, Schema)
	}
	if f.Target != TargetHost {
		return nil, fmt.Errorf("router file: unsupported target %q (want %q)", f.Target, TargetHost)
	}
	if len(f.Aliases) == 0 {
		return nil, errors.New("router file: no aliases")
	}
	seenAlias := make(map[string]bool, len(f.Aliases))
	for ai := range f.Aliases {
		a := &f.Aliases[ai]
		a.Alias = strings.TrimSpace(a.Alias)
		if a.Alias == "" {
			return nil, fmt.Errorf("router file: alias %d has no name", ai+1)
		}
		if lower := strings.ToLower(a.Alias); seenAlias[lower] {
			return nil, fmt.Errorf("router file: alias %q is repeated", a.Alias)
		} else {
			seenAlias[lower] = true
		}
		if err := normaliseCandidates(a); err != nil {
			return nil, err
		}
	}
	return &f, nil
}

func normaliseCandidates(a *AliasEntry) error {
	seen := make(map[string]int, len(a.Candidates))
	for i := range a.Candidates {
		c := &a.Candidates[i]
		c.Model = strings.TrimSpace(c.Model)
		if c.Model == "" {
			return fmt.Errorf("router file: alias %s candidate %d: model is required", a.Alias, i+1)
		}
		if strings.EqualFold(c.Model, a.Alias) {
			return fmt.Errorf("router file: alias %s candidate %d: model %q is the alias itself (the plugin never executes its own alias)", a.Alias, i+1, c.Model)
		}
		if c.ContextWindow < 0 || c.MaxTokens < 0 {
			return fmt.Errorf("router file: alias %s candidate %d (%s): contextWindow and maxTokens must not be negative", a.Alias, i+1, c.Key())
		}
		if q := c.Quota; q != nil {
			for _, v := range []*int{q.RPM, q.RPD, q.TPM, q.TPD} {
				if v != nil && *v < 0 {
					return fmt.Errorf("router file: alias %s candidate %d (%s): quota values must not be negative", a.Alias, i+1, c.Key())
				}
			}
			if q.RPM != nil {
				c.RPM = *q.RPM
			}
			if q.RPD != nil {
				c.RPD = *q.RPD
			}
			if q.TPM != nil {
				c.TPM = *q.TPM
			}
			if q.TPD != nil {
				c.TPD = *q.TPD
			}
		}
		input := make([]string, 0, len(c.Input))
		for _, in := range c.Input {
			if in = strings.ToLower(strings.TrimSpace(in)); in != "" {
				input = append(input, in)
			}
		}
		c.Input = input
		if first, dup := seen[c.Key()]; dup {
			return fmt.Errorf("router file: alias %s candidate %d repeats %s (first at %d)", a.Alias, i+1, c.Key(), first)
		}
		seen[c.Key()] = i + 1
	}
	return nil
}

// AliasStatus summarises one alias for the management status.
type AliasStatus struct {
	Alias      string `json:"alias"`
	Profile    string `json:"profile"`
	Candidates int    `json:"candidates"`
	// Unpriced counts candidates without a usable price (budget math counts them as 0).
	Unpriced int `json:"unpriced"`
}

// Revision identifies the loaded router file: the catalog and quota-source hashes the exporter
// recorded, plus the hash and mtime of the file itself.
type Revision struct {
	Schema            string    `json:"schema,omitempty"`
	Target            string    `json:"target,omitempty"`
	ModelPrefix       string    `json:"modelPrefix,omitempty"`
	CatalogSHA256     string    `json:"catalogSha256,omitempty"`
	QuotaSourceSHA256 string    `json:"quotaSourceSha256,omitempty"`
	Gateway           string    `json:"gateway,omitempty"`
	FileSHA256        string    `json:"fileSha256,omitempty"`
	ModTime           time.Time `json:"modTime,omitzero"`
}

// Status is a snapshot of the store for the management status.
type Status struct {
	Path     string        `json:"path"`
	Revision Revision      `json:"revision"`
	Aliases  []AliasStatus `json:"aliases"`
	LoadedAt time.Time     `json:"loadedAt,omitzero"`
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
	rev      Revision
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
	st := Status{Path: s.path, Revision: s.rev, LoadedAt: s.loadedAt, Aliases: []AliasStatus{}}
	if s.file != nil {
		for _, a := range s.file.Aliases {
			as := AliasStatus{Alias: a.Alias, Profile: a.Profile, Candidates: len(a.Candidates)}
			for _, c := range a.Candidates {
				if c.Unpriced() {
					as.Unpriced++
				}
			}
			st.Aliases = append(st.Aliases, as)
		}
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
	sum := sha256.Sum256(data)
	s.file, s.loadErr, s.loadedAt = file, nil, s.now()
	s.rev = Revision{
		Schema:            file.Schema,
		Target:            file.Target,
		ModelPrefix:       file.ModelPrefix,
		CatalogSHA256:     file.CatalogSHA256,
		QuotaSourceSHA256: file.QuotaSourceSHA256,
		Gateway:           file.Gateway,
		FileSHA256:        hex.EncodeToString(sum[:]),
		ModTime:           info.ModTime().UTC(),
	}
}
