package routerfile

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

const validFile = `{"version":1,"candidates":[
 {"provider":" OpenAI ","model":"gpt-4.1","contextWindow":1000,"supportsTools":true,"input":["Text","IMAGE"],"rpm":2,"rpd":null},
 {"provider":"claude","model":"claude-sonnet","contextWindow":null,"supportsTools":null}
]}`

func TestParse(t *testing.T) {
	t.Run("valid object is normalised", func(t *testing.T) {
		f, err := Parse([]byte(validFile))
		if err != nil {
			t.Fatalf("Parse() error = %v", err)
		}
		if f.Version != SupportedVersion || len(f.Candidates) != 2 {
			t.Fatalf("Parse() = %+v", f)
		}
		got := f.Candidates[0]
		want := Candidate{Provider: "openai", Model: "gpt-4.1", ContextWindow: 1000, SupportsTools: true, Input: []string{"text", "image"}, RPM: 2}
		if got.Provider != want.Provider || got.Model != want.Model || got.ContextWindow != want.ContextWindow ||
			got.SupportsTools != want.SupportsTools || got.RPM != want.RPM || got.RPD != want.RPD ||
			strings.Join(got.Input, ",") != strings.Join(want.Input, ",") {
			t.Errorf("candidate 1 = %+v, want %+v", got, want)
		}
		if !got.HasInput("image") || got.HasInput("audio") {
			t.Errorf("HasInput wrong for %v", got.Input)
		}
		second := f.Candidates[1]
		if second.ContextWindow != 0 || second.SupportsTools || len(second.Input) != 0 {
			t.Errorf("candidate 2 = %+v, want unknown window, no tools, no inputs", second)
		}
	})
	t.Run("bare array is accepted", func(t *testing.T) {
		f, err := Parse([]byte(`[{"provider":"a","model":"m"}]`))
		if err != nil || len(f.Candidates) != 1 || f.Version != SupportedVersion {
			t.Fatalf("Parse() = %+v, %v", f, err)
		}
	})
	t.Run("sample file", func(t *testing.T) {
		data, err := os.ReadFile(filepath.Join("..", "..", "testdata", "router.sample.json"))
		if err != nil {
			t.Fatal(err)
		}
		f, err := Parse(data)
		if err != nil || len(f.Candidates) != 5 {
			t.Fatalf("Parse(sample) = %+v, %v", f, err)
		}
	})

	invalid := []struct{ name, data, want string }{
		{"empty", ``, "empty"},
		{"not json", `{nope`, "router file"},
		{"wrong type", `{"candidates":"x"}`, "router file"},
		{"no candidates", `{"version":1,"candidates":[]}`, "no candidates"},
		{"object without candidates", `{"version":1}`, "no candidates"},
		{"future version", `{"version":2,"candidates":[{"provider":"a","model":"m"}]}`, "unsupported version"},
		{"missing provider", `{"candidates":[{"model":"m"}]}`, "provider and model"},
		{"missing model", `{"candidates":[{"provider":"a"}]}`, "provider and model"},
		{"negative window", `{"candidates":[{"provider":"a","model":"m","contextWindow":-1}]}`, "negative"},
		{"negative rpm", `{"candidates":[{"provider":"a","model":"m","rpm":-1}]}`, "negative"},
		{"duplicate", `{"candidates":[{"provider":"a","model":"m"},{"provider":"A","model":"m"}]}`, "repeats"},
	}
	for _, tc := range invalid {
		t.Run("invalid "+tc.name, func(t *testing.T) {
			f, err := Parse([]byte(tc.data))
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("Parse() = %+v, %v; want error containing %q", f, err, tc.want)
			}
			if f != nil {
				t.Errorf("Parse() returned a file alongside the error")
			}
		})
	}
}

// write replaces path with data and gives it a distinct mtime so the store sees the change even on
// filesystems with coarse timestamps.
func write(t *testing.T, path, data string, mtime time.Time) {
	t.Helper()
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(path, mtime, mtime); err != nil {
		t.Fatal(err)
	}
}

func TestStoreReload(t *testing.T) {
	path := filepath.Join(t.TempDir(), "router.json")
	t0 := time.Date(2026, 11, 27, 10, 0, 0, 0, time.UTC)

	store := NewStore(path)
	if f, err := store.Current(); f != nil || err == nil {
		t.Fatalf("missing file: Current() = %v, %v; want nil file and an error", f, err)
	}

	write(t, path, `{"candidates":[{"provider":"a","model":"one"}]}`, t0)
	f, err := store.Current()
	if err != nil || f == nil || f.Candidates[0].Model != "one" {
		t.Fatalf("first load: %+v, %v", f, err)
	}
	if st := store.Status(); st.Candidates != 1 || st.Error != "" || st.LoadedAt.IsZero() || st.Path != path {
		t.Fatalf("Status() after good load = %+v", st)
	}

	// Unchanged file: the same pointer comes back without a reload.
	if again, _ := store.Current(); again != f {
		t.Errorf("unchanged file was reloaded")
	}

	// A changed file is picked up.
	write(t, path, `{"candidates":[{"provider":"a","model":"two"},{"provider":"b","model":"three"}]}`, t0.Add(time.Minute))
	f2, err := store.Current()
	if err != nil || len(f2.Candidates) != 2 || f2.Candidates[0].Model != "two" {
		t.Fatalf("reload: %+v, %v", f2, err)
	}

	// An invalid new version keeps the previous good one and reports the problem.
	write(t, path, `{"candidates":[{"provider":"a"`, t0.Add(2*time.Minute))
	f3, err := store.Current()
	if err == nil {
		t.Fatalf("invalid file: want an error")
	}
	if f3 != f2 {
		t.Errorf("invalid file replaced the previous good version: %+v", f3)
	}
	if st := store.Status(); st.Error == "" || st.Candidates != 2 {
		t.Errorf("Status() after bad reload = %+v", st)
	}

	// The still-invalid file is not re-read, and the error stays.
	if _, err := store.Current(); err == nil {
		t.Errorf("error vanished without the file changing")
	}

	// Fixing the file clears the error.
	write(t, path, `{"candidates":[{"provider":"c","model":"four"}]}`, t0.Add(3*time.Minute))
	f4, err := store.Current()
	if err != nil || f4.Candidates[0].Model != "four" {
		t.Fatalf("fixed file: %+v, %v", f4, err)
	}
	if st := store.Status(); st.Error != "" {
		t.Errorf("Status().Error = %q after the fix", st.Error)
	}

	// A vanished file keeps serving the last good version and reports the stat error; it recovers.
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	f5, err := store.Current()
	if err == nil || f5 != f4 {
		t.Fatalf("removed file: %+v, %v; want previous good version and an error", f5, err)
	}
	write(t, path, `{"candidates":[{"provider":"c","model":"five"}]}`, t0.Add(4*time.Minute))
	f6, err := store.Current()
	if err != nil || f6.Candidates[0].Model != "five" {
		t.Fatalf("restored file: %+v, %v", f6, err)
	}
}

func TestStoreSameSizeChangeNeedsNewMtime(t *testing.T) {
	// Documents the signature: mtime or size must change for a reload.
	path := filepath.Join(t.TempDir(), "router.json")
	t0 := time.Date(2026, 11, 27, 10, 0, 0, 0, time.UTC)
	write(t, path, `{"candidates":[{"provider":"a","model":"one"}]}`, t0)
	store := NewStore(path)
	f, _ := store.Current()
	write(t, path, `{"candidates":[{"provider":"a","model":"two"}]}`, t0)
	if again, _ := store.Current(); again != f {
		t.Errorf("same size and mtime must not reload")
	}
	write(t, path, `{"candidates":[{"provider":"a","model":"two"}]}`, t0.Add(time.Second))
	if again, _ := store.Current(); again == f || again.Candidates[0].Model != "two" {
		t.Errorf("new mtime must reload, got %+v", again)
	}
}

func TestStoreNoPath(t *testing.T) {
	f, err := NewStore("").Current()
	if f != nil || !errors.Is(err, ErrNoPath) {
		t.Fatalf("Current() = %v, %v; want nil, ErrNoPath", f, err)
	}
}
