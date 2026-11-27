package routerfile

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// file wraps candidate JSON in a minimal valid router file with one alias.
func file(candidates string) string {
	return `{"schema":"tierbridge-router/1","catalogSha256":"abc","aliases":[{"alias":"tierbridge/auto","profile":"free-first","candidates":[` + candidates + `]}]}`
}

const fullCandidate = `{"model":"tb-llama-70b","provider":" Openai-Compatible-GW ","id":"llama-70b","key":"omp-live:gw:gw/llama-70b",
 "contextWindow":131072,"maxTokens":8192,"supportsTools":true,"input":["Text","IMAGE"],
 "cost":{"status":"free","input":0.0,"output":0.0},"confidence":"declared",
 "quota":{"rpm":30,"rpd":14400,"tpm":6000,"tpd":500000,"monthlyTokenBudget":"~30M","source":"freellmapi:groq/llama-70b","matchedBy":"canonicalId","matches":1},
 "reasons":["known-free","freellmapi-limits"],"futureField":{"x":1}}`

func TestParseRealShape(t *testing.T) {
	f, err := Parse([]byte(`{"schema":"tierbridge-router/1","catalogSha256":"cafe","quotaSourceSha256":"absent","gateway":"omp-live:gw","someNewTopLevelKey":[1],
	 "aliases":[{"alias":"tierbridge/auto","profile":"free-first","skipped":{"noPrice":2},"candidates":[` + fullCandidate + `]}]}`))
	if err != nil {
		t.Fatalf("Parse() error = %v", err)
	}
	if f.CatalogSHA256 != "cafe" || f.QuotaSourceSHA256 != "absent" || f.Gateway != "omp-live:gw" {
		t.Errorf("file metadata = %+v", f)
	}
	a := f.Alias(" TierBridge/AUTO ")
	if a == nil || a.Profile != "free-first" || len(a.Candidates) != 1 || string(a.Skipped) != `{"noPrice":2}` {
		t.Fatalf("alias = %+v", a)
	}
	c := a.Candidates[0]
	if c.Provider != "openai-compatible-gw" || c.Model != "tb-llama-70b" || c.ID != "llama-70b" || c.CatalogKey != "omp-live:gw:gw/llama-70b" {
		t.Errorf("identity = %+v", c)
	}
	if c.ContextWindow != 131072 || c.MaxTokens != 8192 || !c.SupportsTools || c.Confidence != "declared" {
		t.Errorf("limits = %+v", c)
	}
	if c.RPM != 30 || c.RPD != 14400 || *c.Quota.TPM != 6000 || *c.Quota.TPD != 500000 || c.Quota.Matches != 1 {
		t.Errorf("quota = %+v (rpm %d rpd %d)", c.Quota, c.RPM, c.RPD)
	}
	if strings.Join(c.Input, ",") != "text,image" || !c.HasInput("image") || c.HasInput("audio") {
		t.Errorf("input = %v", c.Input)
	}
	if c.Unpriced() {
		t.Errorf("a free candidate must be priced")
	}
	if f.Alias("nope") != nil || (*File)(nil).Alias("x") != nil {
		t.Errorf("Alias() must return nil for unknown names and a nil file")
	}
}

func TestParseNullsAndUnpriced(t *testing.T) {
	f, err := Parse([]byte(file(`
	 {"model":"a","provider":"p","contextWindow":null,"maxTokens":null,"supportsTools":null,"input":null,"cost":{"status":"unknown","input":null,"output":null},"quota":null},
	 {"model":"b","provider":"p","cost":{"status":"known","input":1.5,"output":null}},
	 {"model":"c","provider":"p","cost":{"status":"known","input":1.5,"output":6}},
	 {"model":"d","provider":"p","cost":{"status":"free","input":0,"output":0},"quota":{"rpm":null,"rpd":null,"tpm":null,"tpd":null}},
	 {"model":"e","provider":"p"}`)))
	if err != nil {
		t.Fatalf("Parse() error = %v", err)
	}
	cands := f.Aliases[0].Candidates
	wantUnpriced := []bool{true, true, false, false, true}
	for i, c := range cands {
		if c.Unpriced() != wantUnpriced[i] {
			t.Errorf("candidate %s: Unpriced() = %v, want %v", c.Model, c.Unpriced(), wantUnpriced[i])
		}
		if c.RPM != 0 || c.RPD != 0 {
			t.Errorf("candidate %s: null quota must mean unlimited, got rpm %d rpd %d", c.Model, c.RPM, c.RPD)
		}
	}
	a := cands[0]
	if a.ContextWindow != 0 || a.SupportsTools || len(a.Input) != 0 || a.HasInput("text") {
		t.Errorf("null limits = %+v, want unknown window, no tools, no inputs", a)
	}
}

func TestParseSampleFile(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "testdata", "router.sample.json"))
	if err != nil {
		t.Fatal(err)
	}
	f, err := Parse(data)
	if err != nil {
		t.Fatalf("Parse(sample) error = %v", err)
	}
	if len(f.Aliases) != 2 || len(f.Alias("tierbridge/auto").Candidates) != 3 {
		t.Fatalf("sample = %+v", f)
	}
	if empty := f.Alias("tierbridge/empty"); empty == nil || len(empty.Candidates) != 0 {
		t.Errorf("an alias with no candidates must be valid, got %+v", empty)
	}
}

func TestParseInvalid(t *testing.T) {
	const ok = `{"model":"m","provider":"p"}`
	tests := []struct{ name, data, want string }{
		{"empty", ``, "empty"},
		{"not json", `{nope`, "router file"},
		{"wrong type", `{"schema":"tierbridge-router/1","aliases":"x"}`, "router file"},
		{"no schema", `{"aliases":[{"alias":"a","candidates":[]}]}`, "unsupported schema"},
		{"future schema", `{"schema":"tierbridge-router/2","aliases":[{"alias":"a","candidates":[]}]}`, "unsupported schema"},
		{"legacy top-level candidates", `{"candidates":[` + ok + `]}`, "unsupported schema"},
		{"no aliases", `{"schema":"tierbridge-router/1","aliases":[]}`, "no aliases"},
		{"nameless alias", `{"schema":"tierbridge-router/1","aliases":[{"alias":" ","candidates":[]}]}`, "no name"},
		{"repeated alias", `{"schema":"tierbridge-router/1","aliases":[{"alias":"a","candidates":[]},{"alias":"A","candidates":[]}]}`, "repeated"},
		{"missing provider", file(`{"model":"m"}`), "provider and model"},
		{"missing model", file(`{"provider":"p"}`), "provider and model"},
		{"negative window", file(`{"model":"m","provider":"p","contextWindow":-1}`), "negative"},
		{"negative rpm", file(`{"model":"m","provider":"p","quota":{"rpm":-1}}`), "negative"},
		{"duplicate", file(ok + `,{"model":"m","provider":"P"}`), "repeats"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
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

func model(t *testing.T, f *File) string {
	t.Helper()
	if f == nil || f.Alias("tierbridge/auto") == nil || len(f.Alias("tierbridge/auto").Candidates) == 0 {
		t.Fatalf("no candidates in %+v", f)
	}
	return f.Alias("tierbridge/auto").Candidates[0].Model
}

func TestStoreReload(t *testing.T) {
	path := filepath.Join(t.TempDir(), "router.json")
	t0 := time.Date(2026, 11, 27, 10, 0, 0, 0, time.UTC)

	store := NewStore(path)
	if f, err := store.Current(); f != nil || err == nil {
		t.Fatalf("missing file: Current() = %v, %v; want nil file and an error", f, err)
	}

	write(t, path, file(`{"provider":"a","model":"one","cost":{"status":"unknown"}}`), t0)
	f, err := store.Current()
	if err != nil || model(t, f) != "one" {
		t.Fatalf("first load: %+v, %v", f, err)
	}
	st := store.Status()
	if st.Error != "" || st.LoadedAt.IsZero() || st.Path != path || len(st.Aliases) != 1 {
		t.Fatalf("Status() after good load = %+v", st)
	}
	if a := st.Aliases[0]; a.Alias != "tierbridge/auto" || a.Profile != "free-first" || a.Candidates != 1 || a.Unpriced != 1 {
		t.Errorf("alias status = %+v, want 1 unpriced candidate flagged", a)
	}
	rev := st.Revision
	if rev.CatalogSHA256 != "abc" || len(rev.FileSHA256) != 64 || !rev.ModTime.Equal(t0) || rev.Schema != SchemaV1 {
		t.Errorf("revision = %+v", rev)
	}

	// Unchanged file: the same pointer comes back without a reload.
	if again, _ := store.Current(); again != f {
		t.Errorf("unchanged file was reloaded")
	}

	// A changed file is picked up and gets a new revision.
	write(t, path, file(`{"provider":"a","model":"two"},{"provider":"b","model":"three"}`), t0.Add(time.Minute))
	f2, err := store.Current()
	if err != nil || len(f2.Aliases[0].Candidates) != 2 || model(t, f2) != "two" {
		t.Fatalf("reload: %+v, %v", f2, err)
	}
	if rev2 := store.Status().Revision; rev2.FileSHA256 == rev.FileSHA256 {
		t.Errorf("file hash did not change on reload")
	}

	// An invalid new version keeps the previous good one and reports the problem.
	write(t, path, `{"schema":"tierbridge-router/1","aliases":[{"alias":"x"`, t0.Add(2*time.Minute))
	f3, err := store.Current()
	if err == nil {
		t.Fatalf("invalid file: want an error")
	}
	if f3 != f2 {
		t.Errorf("invalid file replaced the previous good version: %+v", f3)
	}
	if st := store.Status(); st.Error == "" || st.Aliases[0].Candidates != 2 {
		t.Errorf("Status() after bad reload = %+v", st)
	}

	// The still-invalid file is not re-read, and the error stays.
	if _, err := store.Current(); err == nil {
		t.Errorf("error vanished without the file changing")
	}

	// Fixing the file clears the error.
	write(t, path, file(`{"provider":"c","model":"four"}`), t0.Add(3*time.Minute))
	f4, err := store.Current()
	if err != nil || model(t, f4) != "four" {
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
	write(t, path, file(`{"provider":"c","model":"five"}`), t0.Add(4*time.Minute))
	f6, err := store.Current()
	if err != nil || model(t, f6) != "five" {
		t.Fatalf("restored file: %+v, %v", f6, err)
	}
}

func TestStoreSameSizeChangeNeedsNewMtime(t *testing.T) {
	// Documents the signature: mtime or size must change for a reload.
	path := filepath.Join(t.TempDir(), "router.json")
	t0 := time.Date(2026, 11, 27, 10, 0, 0, 0, time.UTC)
	write(t, path, file(`{"provider":"a","model":"one"}`), t0)
	store := NewStore(path)
	f, _ := store.Current()
	write(t, path, file(`{"provider":"a","model":"two"}`), t0)
	if again, _ := store.Current(); again != f {
		t.Errorf("same size and mtime must not reload")
	}
	write(t, path, file(`{"provider":"a","model":"two"}`), t0.Add(time.Second))
	if again, _ := store.Current(); again == f || model(t, again) != "two" {
		t.Errorf("new mtime must reload, got %+v", again)
	}
}

func TestStoreNoPath(t *testing.T) {
	f, err := NewStore("").Current()
	if f != nil || !errors.Is(err, ErrNoPath) {
		t.Fatalf("Current() = %v, %v; want nil, ErrNoPath", f, err)
	}
}
