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
	Handled  bool   `json:"handled"`
	Provider string `json:"provider,omitempty"`
	Model    string `json:"model,omitempty"`
	Rank     int    `json:"rank,omitempty"`
	Of       int    `json:"of,omitempty"`
	Reason   string `json:"reason"`
	Skips    []Skip `json:"skips,omitempty"`
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
