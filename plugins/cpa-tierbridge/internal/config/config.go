// Package config parses the plugin's own configuration, the YAML the host hands over in
// plugin.register / plugin.reconfigure (config_yaml). In CLIProxyAPI's config.yaml it lives under
// plugins.configs.tierbridge; the host adds its own "enabled" and "priority" keys, which are ignored.
package config

import (
	"fmt"
	"math"
	"strings"

	"gopkg.in/yaml.v3"
)

// Defaults of the optional keys.
const (
	DefaultAlias           = "tierbridge/auto"
	DefaultCooldownSeconds = 60
	DefaultDecisionsKeep   = 50

	// MaxDecisionsKeep bounds the decisions ring so a typo cannot grow the plugin without limit.
	MaxDecisionsKeep = 10000
)

// Config is the validated plugin configuration.
type Config struct {
	// RouterFile is the path of tierbridge-router.json (the ranking exported by `tierbridge export
	// cpa-router`). Empty means no router file: the alias is never handled.
	RouterFile string `yaml:"router-file"`
	// Alias is the model name the plugin answers to; every other model is left to the host.
	Alias string `yaml:"alias"`
	// DailyBudgetUSD caps the estimated spend per UTC day; 0 means no budget.
	DailyBudgetUSD float64 `yaml:"daily-budget-usd"`
	// CooldownSeconds is how long a candidate that failed upstream is skipped.
	CooldownSeconds int `yaml:"cooldown-seconds"`
	// StatePath is the file counters and decisions persist to; empty means no persistence.
	StatePath string `yaml:"state-path"`
	// DecisionsKeep is the size of the recent-decisions ring; 0 keeps none.
	DecisionsKeep int `yaml:"decisions-keep"`
	// DebugForceError is a TEST SWITCH: it makes RouteModel panic internally so an acceptance run can
	// prove the host falls through when the plugin misbehaves.
	DebugForceError bool `yaml:"debug-force-error"`
}

// Default returns the configuration used before any config_yaml was applied.
func Default() Config {
	return Config{
		Alias:           DefaultAlias,
		CooldownSeconds: DefaultCooldownSeconds,
		DecisionsKeep:   DefaultDecisionsKeep,
	}
}

// Parse decodes and validates config_yaml. Missing keys keep their defaults and unknown keys are
// ignored (the host adds some of its own). On error the zero Config is returned: callers keep their
// previous good configuration.
func Parse(raw []byte) (Config, error) {
	cfg := Default()
	if len(strings.TrimSpace(string(raw))) == 0 {
		return cfg, nil
	}
	if err := yaml.Unmarshal(raw, &cfg); err != nil {
		return Config{}, fmt.Errorf("parse plugin config: %w", err)
	}
	cfg.RouterFile = strings.TrimSpace(cfg.RouterFile)
	cfg.StatePath = strings.TrimSpace(cfg.StatePath)
	cfg.Alias = strings.TrimSpace(cfg.Alias)
	if cfg.Alias == "" {
		cfg.Alias = DefaultAlias
	}
	if !(cfg.DailyBudgetUSD >= 0) || math.IsInf(cfg.DailyBudgetUSD, 0) {
		return Config{}, fmt.Errorf("daily-budget-usd must be a finite number >= 0, got %v", cfg.DailyBudgetUSD)
	}
	if cfg.CooldownSeconds < 0 {
		return Config{}, fmt.Errorf("cooldown-seconds must be >= 0, got %d", cfg.CooldownSeconds)
	}
	if cfg.DecisionsKeep < 0 || cfg.DecisionsKeep > MaxDecisionsKeep {
		return Config{}, fmt.Errorf("decisions-keep must be between 0 and %d, got %d", MaxDecisionsKeep, cfg.DecisionsKeep)
	}
	return cfg, nil
}
