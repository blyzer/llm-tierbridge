package rpc

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

// hostUsageRecord serialises a usage.handle request the way the host does: pluginapi.UsageRecord has no
// json tags, so every key is the capitalised Go field name, ResponseHeaders is an http.Header, RequestedAt
// a time and Latency a duration in nanoseconds.
func hostUsageRecord(t *testing.T, rec map[string]any) []byte {
	t.Helper()
	base := map[string]any{
		"RequestID": "r-1", "Provider": "gw", "Model": "", "Alias": "", "Generate": true,
		"RequestedAt": clock, "Latency": int64(1500 * time.Millisecond), "Failed": false,
		"Failure":         map[string]any{"StatusCode": 0, "Body": ""},
		"Detail":          map[string]any{"InputTokens": 0, "OutputTokens": 0, "TotalTokens": 0, "ReasoningTokens": 0},
		"ResponseHeaders": http.Header{},
	}
	for k, v := range rec {
		base[k] = v
	}
	raw, err := wire.Marshal(base)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func detail(in, out, total int) map[string]any {
	return map[string]any{"InputTokens": in, "OutputTokens": out, "TotalTokens": total}
}

func sendUsage(t *testing.T, h *Handler, raw []byte) {
	t.Helper()
	if got := string(h.Handle(wire.MethodUsageHandle, raw)); got != `{"ok":true,"result":{}}` {
		t.Fatalf("usage.handle = %s, want an empty success", got)
	}
}

const twoModels = `{"model":"tb-a","quota":{"tpm":1000,"tpd":1500},"cost":{"status":"free","input":0,"output":0}},
 {"model":"tb-b","cost":{"status":"known","input":1,"output":2}}`

func TestUsageChargesTokensAgainstTheModelsQuota(t *testing.T) {
	h, _ := newPlugin(t, wrap(twoModels), "")
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-a", "Detail": detail(700, 200, 900)}))
	// 900 of 1000 tokens per minute used: a 400-token prompt (1600 bytes) no longer fits tb-a.
	got := route(t, h, aliasModel, chatBody(1600, ""), nil)
	if got.TargetModel != "tb-b" || got.Reason != "rank 2 of 2 (skipped 1: tb-a tpm)" {
		t.Errorf("route = %+v", got)
	}
	if small := route(t, h, aliasModel, chatBody(40, ""), nil); small.TargetModel != "tb-a" {
		t.Errorf("a 10-token prompt still fits: %+v", small)
	}
}

func TestUsageMatchesByAliasAndIgnoresCase(t *testing.T) {
	h, _ := newPlugin(t, wrap(twoModels), "")
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "upstream-id", "Alias": "tb-a", "Detail": detail(0, 0, 1400)}))
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "TB-A", "Detail": detail(0, 0, 100)}))
	got := h.countersView()
	if got.Models[0].TPD.Used != 1500 {
		t.Errorf("tpd used = %d, want 1500 (alias match plus case-insensitive match)", got.Models[0].TPD.Used)
	}
	seen := h.Status().LastUsageModels
	if len(seen) != 2 || !seen[0].Matched || !seen[1].Matched || seen[1].Model != "upstream-id" || seen[1].Alias != "tb-a" {
		t.Errorf("lastUsageModels = %+v", seen)
	}
}

func TestUsageForAnUnknownModelIsIgnored(t *testing.T) {
	h, _ := newPlugin(t, wrap(twoModels), "daily-budget-usd: 1\n")
	before := h.countersView()
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "someone-elses", "Alias": "nope", "Detail": detail(9_000_000, 9_000_000, 18_000_000),
		"Failed": true, "Failure": map[string]any{"StatusCode": 429}}))
	if after := h.countersView(); !reflect.DeepEqual(after.Models, before.Models) {
		t.Errorf("counters changed:\n%+v\n%+v", before.Models, after.Models)
	}
	if b := h.budgetView(); b.SpentUSD != 0 {
		t.Errorf("spent = %v", b.SpentUSD)
	}
	// ...but it is still visible in lastUsageModels, unmatched, so the controller can compare names.
	seen := h.Status().LastUsageModels
	if len(seen) != 1 || seen[0].Matched || seen[0].Model != "someone-elses" || seen[0].Status != 429 || !seen[0].Failed || seen[0].Tokens != 18_000_000 {
		t.Errorf("lastUsageModels = %+v", seen)
	}
}

func TestUsageWithoutARouterFileIsIgnored(t *testing.T) {
	h, _ := newPlugin(t, "", "")
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-a", "Detail": detail(1, 1, 2)}))
	if seen := h.Status().LastUsageModels; len(seen) != 1 || seen[0].Matched {
		t.Errorf("lastUsageModels = %+v", seen)
	}
}

func TestUsageCooldownFrom429AndRetryAfter(t *testing.T) {
	h, _ := newPlugin(t, wrap(twoModels), "cooldown-seconds: 30\n")
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-a", "Failed": true, "Failure": map[string]any{"StatusCode": 429, "Body": "slow down"}}))
	got := route(t, h, aliasModel, chatBody(40, ""), nil)
	if got.TargetModel != "tb-b" || got.Reason != "rank 2 of 2 (skipped 1: tb-a cooldown)" {
		t.Fatalf("route = %+v, want tb-a cooling down", got)
	}
	if c := h.countersView().Models[0]; c.CooldownRemainingSeconds != 30 {
		t.Errorf("cooldown remaining = %d, want 30", c.CooldownRemainingSeconds)
	}
	h.now = func() time.Time { return clock.Add(31 * time.Second) }
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.TargetModel != "tb-a" {
		t.Errorf("after the cooldown: %+v", got)
	}

	// A larger Retry-After wins over cooldown-seconds.
	h.now = func() time.Time { return clock }
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-b", "Failed": true, "Failure": map[string]any{"StatusCode": 503},
		"ResponseHeaders": http.Header{"Retry-After": {"120"}}}))
	if c := h.countersView().Models[1]; c.CooldownRemainingSeconds != 120 {
		t.Errorf("cooldown remaining = %d, want 120 from Retry-After", c.CooldownRemainingSeconds)
	}
}

func TestUsageHandleNeverFailsOnGarbage(t *testing.T) {
	h, _ := newPlugin(t, wrap(twoModels), "")
	for name, raw := range map[string]string{
		"empty":           "",
		"whitespace":      "  \n",
		"not json":        "{{{{",
		"truncated":       `{"Model":"tb-a","Detail":{"InputTokens":`,
		"array":           `[1,2,3]`,
		"string":          `"hello"`,
		"null":            `null`,
		"number":          `42`,
		"binary":          "\x00\xff\xfe",
		"wrong types":     `{"Model":5,"Failed":"yes","Detail":"x","Failure":[],"ResponseHeaders":"h"}`,
		"huge tokens":     `{"Model":"tb-a","Detail":{"TotalTokens":9223372036854775807}}`,
		"negative tokens": `{"Model":"tb-a","Detail":{"TotalTokens":-5,"InputTokens":-9}}`,
		"string headers":  `{"Model":"tb-a","Failed":true,"Failure":{"StatusCode":429},"ResponseHeaders":{"Retry-After":"7"}}`,
		"huge status":     `{"Model":"tb-a","Failed":true,"Failure":{"StatusCode":2147483647}}`,
		"deep":            strings.Repeat(`{"a":`, 200) + "1" + strings.Repeat("}", 200),
	} {
		t.Run(name, func(t *testing.T) {
			if got := string(h.Handle(wire.MethodUsageHandle, []byte(raw))); got != `{"ok":true,"result":{}}` {
				t.Errorf("usage.handle(%q) = %s", raw, got)
			}
		})
	}
	if h.Handle(wire.MethodUsageHandle, nil) == nil {
		t.Error("nil request produced no response")
	}
	// The counters are still sane after the garbage: tb-a cooled down (a 429 and a 5xx record were in
	// the mix), tb-b serves.
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.TargetModel != "tb-b" {
		t.Errorf("route = %+v", got)
	}
}

func TestUsageHandleSurvivesAPanicInsideTheHandler(t *testing.T) {
	h, _ := newPlugin(t, wrap(twoModels), "")
	h.acct = nil // makes a matched record dereference nil
	if got := string(h.Handle(wire.MethodUsageHandle, hostUsageRecord(t, map[string]any{"Model": "tb-a"}))); got != `{"ok":true,"result":{}}` {
		t.Errorf("usage.handle = %s, want success even after an internal panic", got)
	}
	if !strings.Contains(h.Status().LastError, "usage.handle: panic") {
		t.Errorf("the panic was not kept as the last error: %q", h.Status().LastError)
	}
}

func TestLastUsageModelsIsABoundedRingNewestFirst(t *testing.T) {
	h, _ := newPlugin(t, wrap(twoModels), "")
	for i := range MaxUsageSeen + 5 {
		sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": fmt.Sprintf("m%d", i), "Provider": "gw", "Detail": detail(1, 1, 2)}))
	}
	seen := h.Status().LastUsageModels
	if len(seen) != MaxUsageSeen || seen[0].Model != fmt.Sprintf("m%d", MaxUsageSeen+4) || seen[len(seen)-1].Model != "m5" {
		t.Fatalf("ring = %d entries, first %q last %q", len(seen), seen[0].Model, seen[len(seen)-1].Model)
	}
	if seen[0].Provider != "gw" || seen[0].Tokens != 2 || seen[0].Failed || seen[0].Status != 0 || seen[0].Time != clock {
		t.Errorf("entry = %+v", seen[0])
	}
}

func TestStatePersistsAcrossHandlers(t *testing.T) {
	state := filepath.Join(t.TempDir(), "tierbridge-state.json")
	yaml := fmt.Sprintf("state-path: %q\ncooldown-seconds: 60\n", state)
	h, router := newPlugin(t, wrap(twoModels), yaml)
	route(t, h, aliasModel, chatBody(40, ""), nil)
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-a", "Detail": detail(0, 0, 321)}))
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-b", "Failed": true, "Failure": map[string]any{"StatusCode": 429}}))
	h.Handle(wire.MethodPluginShutdown, nil)
	if info, err := os.Stat(state); err != nil || info.Mode().Perm() != 0o600 {
		t.Fatalf("state file after shutdown: %v %v", info, err)
	}

	h2 := NewHandler()
	h2.now = func() time.Time { return clock.Add(10 * time.Second) }
	cfg := fmt.Sprintf("router-file: %q\nstate-path: %q\ncooldown-seconds: 60\n", router, state)
	h2.Handle(wire.MethodPluginRegister, lifecycle(t, cfg))
	c := h2.countersView().Models
	if c[0].RPM.Used != 1 || c[0].TPD.Used != 321 || c[1].CooldownRemainingSeconds != 50 {
		t.Errorf("counters after restart = %+v", c)
	}
	if p := h2.Status().Persistence; !p.Loaded || p.LoadError != "" {
		t.Errorf("persistence = %+v", p)
	}
}

func TestCorruptedStateFileIsReportedInStatus(t *testing.T) {
	state := filepath.Join(t.TempDir(), "state.json")
	if err := os.WriteFile(state, []byte("{{garbage"), 0o600); err != nil {
		t.Fatal(err)
	}
	h, _ := newPlugin(t, wrap(twoModels), fmt.Sprintf("state-path: %q\n", state))
	p := h.Status().Persistence
	if p.Loaded || !p.Enabled || !strings.Contains(p.LoadError, "corrupted") {
		t.Errorf("persistence = %+v", p)
	}
	if c := h.countersView().Models; c[0].RPM.Used != 0 || c[0].TPD.Used != 0 {
		t.Errorf("counters did not start empty: %+v", c)
	}
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); !got.Handled {
		t.Errorf("routing broke on a corrupted state file: %+v", got)
	}
}

const withIDs = `{"id":"alpha","model":"tb-alpha","cost":{"status":"free","input":0,"output":0}},
 {"id":"alpha","model":"tb-alpha2","cost":{"status":"free","input":0,"output":0}},
 {"id":"beta","model":"tb-beta","cost":{"status":"free","input":0,"output":0}}`

// The host reports the upstream id ("beta"), not the exported model name ("tb-beta"), in UsageRecord.Model.
func TestUsageMatchesTheUpstreamIDWithoutThePrefix(t *testing.T) {
	h, _ := newPlugin(t, wrap(withIDs), "cooldown-seconds: 30\n")
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "beta", "Provider": "gw", "Detail": detail(0, 0, 77)}))
	if c := h.countersView().Models[2]; c.TPD.Used != 77 {
		t.Errorf("tb-beta tpd = %d, want 77 charged through id %q", c.TPD.Used, "beta")
	}
	// Candidates carry no provider (router file /2), so a shared id is ambiguous and charges nothing,
	// whatever provider the record names; the unique id is charged even under another provider.
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "alpha", "Provider": "gw", "Detail": detail(0, 0, 5)}))
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "beta", "Provider": "elsewhere", "Detail": detail(0, 0, 5)}))
	if c := h.countersView().Models; c[0].TPD.Used != 0 || c[1].TPD.Used != 0 || c[2].TPD.Used != 82 {
		t.Errorf("ambiguous id was charged or the unique id was not: %+v", c)
	}
	// The exported model name is never ambiguous: a failed record cools the routed candidate.
	sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-alpha", "Provider": "gw", "Failed": true, "Failure": map[string]any{"StatusCode": 429}}))
	if got := route(t, h, aliasModel, chatBody(40, ""), nil); got.TargetModel != "tb-alpha2" || !strings.Contains(got.Reason, "tb-alpha cooldown") {
		t.Errorf("route = %+v, want tb-alpha cooling down", got)
	}
	seen := h.Status().LastUsageModels
	if seen[len(seen)-1].Model != "beta" || seen[len(seen)-1].Provider != "gw" || !seen[len(seen)-1].Matched {
		t.Errorf("lastUsageModels = %+v", seen)
	}
}

func stateTPD(t *testing.T, path, model string) int64 {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("state file: %v", err)
	}
	var st struct {
		Models map[string]struct{ TPD int64 }
	}
	if err := json.Unmarshal(data, &st); err != nil {
		t.Fatal(err)
	}
	return st.Models[model].TPD
}

// Pending state younger than the debounce must reach the file on quiesce, on shutdown and from the C hook.
func TestFlushOnQuiesceShutdownAndHook(t *testing.T) {
	for _, tc := range []struct {
		name  string
		flush func(h *Handler)
	}{
		{"quiesce", func(h *Handler) { h.Handle(wire.MethodPluginQuiesce, nil) }},
		{"shutdown", func(h *Handler) { h.Handle(wire.MethodPluginShutdown, nil) }},
		{"hook func", func(h *Handler) { h.Flush() }},
		{"hook then shutdown", func(h *Handler) { h.Flush(); h.Flush(); h.Handle(wire.MethodPluginShutdown, nil) }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			state := filepath.Join(t.TempDir(), "state.json")
			h, _ := newPlugin(t, wrap(twoModels), fmt.Sprintf("state-path: %q\n", state))
			// Pin the clock: the first change writes at once, later ones wait for the debounce timer.
			sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-a", "Detail": detail(0, 0, 10)}))
			h.persist.Flush()
			sendUsage(t, h, hostUsageRecord(t, map[string]any{"Model": "tb-a", "Detail": detail(0, 0, 25)}))
			if !h.Status().Persistence.Dirty {
				t.Skip("the debounce timer already wrote the change")
			}
			tc.flush(h)
			if got := stateTPD(t, state, "tb-a"); got != 35 {
				t.Errorf("tpd in the file = %d, want 35", got)
			}
			if h.Status().Persistence.Dirty {
				t.Error("still dirty after the flush")
			}
		})
	}
}

func TestFlushWithoutStatePathAndAfterPanic(t *testing.T) {
	h := NewHandler()
	h.Flush() // no path: nothing to do
	h.persist = nil
	h.Flush() // a nil persister panics inside; Flush recovers
	h.mu.RLock()
	last := h.lastErr
	h.mu.RUnlock()
	if !strings.Contains(last, "flush: panic") {
		t.Errorf("lastError = %q", last)
	}
}

// Flushing from other goroutines while routes and usage records run must neither deadlock nor race.
func TestFlushConcurrentWithRouteAndUsage(t *testing.T) {
	state := filepath.Join(t.TempDir(), "state.json")
	h, _ := newPlugin(t, wrap(twoModels), fmt.Sprintf("state-path: %q\n", state))
	h.now = time.Now
	body := hostRouteRequest(t, aliasModel, chatBody(40, ""), nil)
	rec := hostUsageRecord(t, map[string]any{"Model": "tb-a", "Detail": detail(1, 1, 2)})
	done := make(chan struct{})
	var wg sync.WaitGroup
	for range 3 {
		wg.Add(3)
		go func() {
			defer wg.Done()
			for range 200 {
				h.Handle(wire.MethodModelRoute, body)
			}
		}()
		go func() {
			defer wg.Done()
			for range 200 {
				h.Handle(wire.MethodUsageHandle, rec)
			}
		}()
		go func() {
			defer wg.Done()
			for range 100 {
				h.Flush()
				h.Handle(wire.MethodPluginQuiesce, nil)
			}
		}()
	}
	go func() { wg.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(20 * time.Second):
		t.Fatal("deadlock: route, usage and flush did not finish")
	}
	h.Flush()
	if got := stateTPD(t, state, "tb-a"); got != 600*2 {
		t.Errorf("tpd = %d, want %d", got, 600*2)
	}
}
