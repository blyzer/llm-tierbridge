package config

import (
	"strings"
	"testing"
)

func TestParse(t *testing.T) {
	tests := []struct {
		name    string
		yaml    string
		want    Config
		wantErr string
	}{
		{
			name: "empty gives defaults",
			yaml: "",
			want: Config{Alias: "tierbridge/auto", CooldownSeconds: 60, DecisionsKeep: 50, MaxAttempts: 3, DeadlineSeconds: 120},
		},
		{
			name: "host-only keys are ignored",
			yaml: "enabled: true\npriority: 3\n",
			want: Config{Alias: "tierbridge/auto", CooldownSeconds: 60, DecisionsKeep: 50, MaxAttempts: 3, DeadlineSeconds: 120},
		},
		{
			name: "all keys",
			yaml: "enabled: true\nrouter-file: /etc/tb/router.json\nalias: my/auto\ndaily-budget-usd: 5\ncooldown-seconds: 5\nstate-path: /var/tb/state.json\ndecisions-keep: 7\nmax-attempts: 2\ndeadline-seconds: 9\ndebug-force-error: true\n",
			want: Config{RouterFile: "/etc/tb/router.json", Alias: "my/auto", DailyBudgetUSD: 5, CooldownSeconds: 5, StatePath: "/var/tb/state.json", DecisionsKeep: 7, MaxAttempts: 2, DeadlineSeconds: 9, DebugForceError: true},
		},
		{
			name: "fractional budget and blank alias",
			yaml: "alias: \"  \"\ndaily-budget-usd: 2.5\ndecisions-keep: 0\n",
			want: Config{Alias: "tierbridge/auto", DailyBudgetUSD: 2.5, CooldownSeconds: 60, DecisionsKeep: 0, MaxAttempts: 3, DeadlineSeconds: 120},
		},
		{name: "negative budget", yaml: "daily-budget-usd: -1\n", wantErr: "daily-budget-usd"},
		{name: "nan budget", yaml: "daily-budget-usd: .nan\n", wantErr: "daily-budget-usd"},
		{name: "negative cooldown", yaml: "cooldown-seconds: -1\n", wantErr: "cooldown-seconds"},
		{name: "decisions-keep too big", yaml: "decisions-keep: 1000000\n", wantErr: "decisions-keep"},
		{name: "zero max-attempts", yaml: "max-attempts: 0\n", wantErr: "max-attempts"},
		{name: "huge max-attempts", yaml: "max-attempts: 51\n", wantErr: "max-attempts"},
		{name: "negative deadline", yaml: "deadline-seconds: -1\n", wantErr: "deadline-seconds"},
		{name: "deadline zero means none", yaml: "deadline-seconds: 0\n", want: Config{Alias: "tierbridge/auto", CooldownSeconds: 60, DecisionsKeep: 50, MaxAttempts: 3}},
		{name: "wrong type", yaml: "cooldown-seconds: soon\n", wantErr: "parse plugin config"},
		{name: "not yaml", yaml: "router-file: [unterminated\n", wantErr: "parse plugin config"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, err := Parse([]byte(tc.yaml))
			if tc.wantErr != "" {
				if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
					t.Fatalf("Parse() error = %v, want it to contain %q", err, tc.wantErr)
				}
				if got != (Config{}) {
					t.Errorf("Parse() = %+v on error, want zero Config", got)
				}
				return
			}
			if err != nil {
				t.Fatalf("Parse() error = %v", err)
			}
			if got != tc.want {
				t.Errorf("Parse() = %+v, want %+v", got, tc.want)
			}
		})
	}
}
