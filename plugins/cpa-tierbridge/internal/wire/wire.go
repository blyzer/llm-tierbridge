// Package wire holds the plugin's own copy of the CLIProxyAPI plugin RPC wire types.
//
// The plugin deliberately does not import CLIProxyAPI: the host's own examples need a local replace
// of the host module plus a replace of its JSON library (MoeForge/moego), which a shipped build must
// not depend on. The shapes below therefore mirror the host's sdk/pluginabi and sdk/pluginapi
// packages by hand. The host marshals those Go structs without json
// tags, so their keys are the capitalised Go field names ("Handled", "TargetKind", ...), and []byte
// values travel as base64; only the lifecycle/registration/envelope structs carry snake_case tags.
//
// # Host pin
//
// The contract mirrored here is the one of CLIProxyAPI commit 47f18e4d82bd72f7d2b5ecf5ede93e4837a4625e
// (first contained in release v8.1.7): ABIVersion 1, SchemaVersion 10. The native ABI and the RPC
// schema change often upstream, so the plugin must be rebuilt and re-verified against every new host
// version before it is deployed; bump HostCommit, ABIVersion and SchemaVersion together with the
// golden JSON in the tests.
package wire

import (
	"bytes"
	"encoding/json"
)

// Host pin, see the package comment.
const (
	// HostCommit is the CLIProxyAPI commit whose plugin contract this package mirrors.
	HostCommit = "47f18e4d82bd72f7d2b5ecf5ede93e4837a4625e"
	// ABIVersion is pluginabi.ABIVersion: the native C ABI shape (cliproxy_plugin_api).
	ABIVersion uint32 = 1
	// SchemaVersion is pluginabi.SchemaVersion: the RPC JSON contract negotiated at plugin.register.
	SchemaVersion uint32 = 10
)

// RPC method names (pluginabi.Method*).
const (
	MethodPluginRegister    = "plugin.register"
	MethodPluginReconfigure = "plugin.reconfigure"
	MethodPluginQuiesce     = "plugin.quiesce"
	MethodPluginShutdown    = "plugin.shutdown"

	MethodModelRoute = "model.route"

	MethodUsageHandle = "usage.handle"

	MethodManagementRegister = "management.register"
	MethodManagementHandle   = "management.handle"
)

// ModelRouteTargetProvider is pluginapi.ModelRouteTargetProvider: route through the host's built-in
// auth/executor path.
const ModelRouteTargetProvider = "provider"

// Envelope is pluginabi.Envelope, the response wrapper of every RPC call.
type Envelope struct {
	OK     bool            `json:"ok"`
	Result json.RawMessage `json:"result,omitempty"`
	Error  *Error          `json:"error,omitempty"`
}

// Error is the subset of pluginabi.Error this plugin emits.
type Error struct {
	Code       string `json:"code"`
	Message    string `json:"message"`
	HTTPStatus int    `json:"http_status,omitempty"`
}

// Error codes used in failed envelopes.
const (
	ErrorCodeUnknownMethod = "unknown_method"
	ErrorCodeBadRequest    = "bad_request"
	ErrorCodeInternal      = "internal_error"
)

// LifecycleRequest is the request of plugin.register and plugin.reconfigure (rpcLifecycleRequest).
// ConfigYAML is base64 on the wire; it holds the plugin's own configuration (plugins.configs.<id>)
// plus the host's "enabled" and "priority" keys.
type LifecycleRequest struct {
	ConfigYAML         []byte `json:"config_yaml"`
	SchemaVersion      uint32 `json:"schema_version"`
	MultiAuthProviders bool   `json:"multi_auth_providers,omitempty"`
}

// Registration is the result of plugin.register and plugin.reconfigure (rpcRegistration).
type Registration struct {
	SchemaVersion uint32       `json:"schema_version"`
	Metadata      Metadata     `json:"metadata"`
	Capabilities  Capabilities `json:"capabilities"`
}

// Metadata is pluginapi.Metadata (no json tags: capitalised keys).
type Metadata struct {
	Name             string
	Version          string
	Author           string
	GitHubRepository string
	Logo             string
	ConfigFields     []ConfigField
}

// Config field types (pluginapi.ConfigFieldType*).
const (
	ConfigFieldString  = "string"
	ConfigFieldNumber  = "number"
	ConfigFieldInteger = "integer"
	ConfigFieldBoolean = "boolean"
)

// ConfigField is pluginapi.ConfigField (no json tags: capitalised keys).
type ConfigField struct {
	Name        string
	Type        string
	Default     any
	EnumValues  []string
	Description string
}

// Capabilities is the subset of rpcCapabilities this plugin declares.
type Capabilities struct {
	ModelRouter   bool `json:"model_router"`
	UsagePlugin   bool `json:"usage_plugin"`
	ManagementAPI bool `json:"management_api"`
}

// ModelRouteRequest is the part of pluginapi.ModelRouteRequest (plus the host's host_callback_id)
// the plugin reads; the headers, query, metadata and plugin descriptor are ignored.
type ModelRouteRequest struct {
	PluginID           string
	SourceFormat       string
	RequestedModel     string
	Stream             bool
	Body               []byte
	AvailableProviders []string
}

// ModelRouteResponse is pluginapi.ModelRouteResponse (no json tags: capitalised keys).
type ModelRouteResponse struct {
	Handled     bool
	TargetKind  string
	Target      string
	TargetModel string
	Reason      string
}

// ManagementRegistrationResponse is rpcManagementRegistrationResponse.
type ManagementRegistrationResponse struct {
	Routes    []ManagementRoute `json:"routes"`
	Resources []ResourceRoute   `json:"resources"`
}

// ManagementRoute is pluginapi.ManagementRoute without its in-process Handler.
type ManagementRoute struct {
	Method      string
	Path        string
	Menu        string
	Description string
}

// ResourceRoute is pluginapi.ResourceRoute without its in-process Handler.
type ResourceRoute struct {
	Path        string
	Menu        string
	Description string
}

// ManagementResponse is pluginapi.ManagementResponse. Body is base64 on the wire.
type ManagementResponse struct {
	StatusCode int
	Headers    map[string][]string
	Body       []byte
}

// Marshal encodes v as compact JSON without HTML escaping (the host reads the bytes back with a
// standard decoder, so "<", ">" and "&" in model ids or reasons stay readable in logs and tests).
func Marshal(v any) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimRight(buf.Bytes(), "\n"), nil
}

// OKEnvelope wraps result in a successful envelope.
func OKEnvelope(result any) ([]byte, error) {
	raw, err := Marshal(result)
	if err != nil {
		return nil, err
	}
	return Marshal(Envelope{OK: true, Result: raw})
}

// ErrorEnvelope builds a failed envelope. It never fails: the inputs are plain strings.
func ErrorEnvelope(code, message string) []byte {
	raw, err := Marshal(Envelope{OK: false, Error: &Error{Code: code, Message: message}})
	if err != nil {
		return []byte(`{"ok":false,"error":{"code":"internal_error","message":"encode failure"}}`)
	}
	return raw
}
