package selector

import (
	"sync"
	"time"
)

// Decision is one routing decision for an alias request, kept for the management status.
type Decision struct {
	Time           time.Time `json:"time"`
	RequestedModel string    `json:"requestedModel"`
	EstTokens      int       `json:"estTokens"`
	Tools          bool      `json:"tools"`
	Image          bool      `json:"image"`
	// Handled is false when the plugin left the request to the host (nothing fits, or an error).
	Handled bool `json:"handled"`
	// Model is the candidate that answered (or, when none did, the last one tried).
	Model string `json:"model,omitempty"`
	Rank  int    `json:"rank,omitempty"`
	Of    int    `json:"of,omitempty"`
	// Status is the HTTP status of the final answer (executed requests only).
	Status int `json:"status,omitempty"`
	// Unpriced marks a pick whose price is unknown: budget math counts it as 0.
	Unpriced bool   `json:"unpriced,omitempty"`
	Reason   string `json:"reason"`
	Skips    []Skip `json:"skips,omitempty"`
	// Attempts are the host.model.execute calls the executor made, in order (executed requests only).
	Attempts []Attempt `json:"attempts,omitempty"`
}

// Attempt is one host.model.execute call of the executor.
type Attempt struct {
	Rank  int    `json:"rank"`
	Model string `json:"model"`
	// Status is the HTTP status the host answered with (0: no answer, e.g. a transport error or timeout).
	Status int `json:"status"`
	// OK is true for the attempt whose answer was returned to the client.
	OK bool `json:"ok"`
	// Error is a short description of the failure ("" for a success).
	Error string `json:"error,omitempty"`
	// Reason names why the candidate failed when the status alone does not say: "model_not_found" is
	// a candidate the host does not serve (a stale router file), cooled for cooldown-seconds.
	Reason string `json:"reason,omitempty"`
	// Fallback tells whether the executor moved on to the next candidate after this attempt.
	Fallback bool `json:"fallback,omitempty"`
	// CoolingDown tells whether the failure put the candidate on cooldown.
	CoolingDown bool  `json:"coolingDown,omitempty"`
	DurationMs  int64 `json:"durationMs"`
}

// Recorder is a fixed-size ring of the most recent decisions.
type Recorder struct {
	mu    sync.Mutex
	keep  int
	buf   []Decision // ring storage, len == keep once the first decision is added
	start int        // index of the oldest decision
	n     int        // number of decisions held
}

// NewRecorder creates a ring that keeps the last keep decisions (0 keeps none).
func NewRecorder(keep int) *Recorder {
	return &Recorder{keep: max(keep, 0)}
}

// SetKeep resizes the ring, dropping the oldest decisions when it shrinks.
func (r *Recorder) SetKeep(keep int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	keep = max(keep, 0)
	oldest := r.oldestFirstLocked()
	if len(oldest) > keep {
		oldest = oldest[len(oldest)-keep:]
	}
	r.keep, r.start, r.n, r.buf = keep, 0, len(oldest), nil
	if keep > 0 && len(oldest) > 0 {
		r.buf = make([]Decision, keep)
		copy(r.buf, oldest)
	}
}

// Add appends a decision, evicting the oldest one when the ring is full.
func (r *Recorder) Add(d Decision) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.keep == 0 {
		return
	}
	if r.buf == nil {
		r.buf = make([]Decision, r.keep)
	}
	if r.n < r.keep {
		r.buf[(r.start+r.n)%r.keep] = d
		r.n++
		return
	}
	r.buf[r.start] = d
	r.start = (r.start + 1) % r.keep
}

// Recent returns the kept decisions, newest first.
func (r *Recorder) Recent() []Decision {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := r.oldestFirstLocked()
	for i, j := 0, len(out)-1; i < j; i, j = i+1, j-1 {
		out[i], out[j] = out[j], out[i]
	}
	return out
}

func (r *Recorder) oldestFirstLocked() []Decision {
	out := make([]Decision, r.n)
	for i := range out {
		out[i] = r.buf[(r.start+i)%r.keep]
	}
	return out
}
