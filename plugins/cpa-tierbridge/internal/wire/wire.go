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

	MethodExecutorIdentifier  = "executor.identifier"
	MethodExecutorExecute     = "executor.execute"
	MethodExecutorCountTokens = "executor.count_tokens"
	MethodExecutorHTTP        = "executor.http_request"
	// MethodExecutorStream is declared by the contract but not served yet: the first increment of the
	// self executor is non-streaming only, so streaming requests are answered with a clear error.
	MethodExecutorStream = "executor.execute_stream"

	MethodUsageHandle = "usage.handle"

	MethodManagementRegister = "management.register"
	MethodManagementHandle   = "management.handle"
)

// Host callback methods the plugin issues (pluginabi.MethodHost*).
const (
	MethodHostModelExecute       = "host.model.execute"
	MethodHostModelExecuteStream = "host.model.execute_stream"
	MethodHostModelStreamRead    = "host.model.stream_read"
	MethodHostModelStreamClose   = "host.model.stream_close"
	MethodHostStreamEmit         = "host.stream.emit"
	MethodHostStreamClose        = "host.stream.close"
)

// ModelRouteTargetSelf is pluginapi.ModelRouteTargetSelf: the host runs the request through this
// plugin's own executor (executor.execute), which then calls back into the host per candidate.
const ModelRouteTargetSelf = "self"

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
	Retryable  bool   `json:"retryable,omitempty"`
	// RetryAfterSeconds is an optional hint; hosts that do not know it ignore it.
	RetryAfterSeconds int `json:"retry_after_seconds,omitempty"`
}

// Error codes used in failed envelopes.
const (
	ErrorCodeUnknownMethod = "unknown_method"
	ErrorCodeBadRequest    = "bad_request"
	ErrorCodeInternal      = "internal_error"

	// ErrorCodeNotImplemented is pluginabi.ErrorCodeNotImplemented: an optional capability is
	// unsupported, so the host keeps the model available instead of cooling it.
	ErrorCodeNotImplemented = "not_implemented"
	// ErrorCodeRequestScoped is pluginabi.ErrorCodeRequestScoped: the request itself is at fault, so
	// the host skips cooldown and stops rotating credentials.
	ErrorCodeRequestScoped = "request_scoped"
	// ErrorCodeTransientRateLimit is pluginabi.ErrorCodeTransientRateLimit (a 429 that cools only the
	// reporting model).
	ErrorCodeTransientRateLimit = "transient_rate_limit"
	// Codes of this plugin's own failures (no host meaning: the host maps them by http_status).
	ErrorCodeUpstream = "upstream_error"
	ErrorCodeNoRoute  = "no_candidate"
	ErrorCodeDeadline = "deadline_exceeded"
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
	ModelRouter bool `json:"model_router"`
	// Executor makes the host call executor.* on this plugin. The model scope must be "static" (the
	// plugin serves a model alias, not an OAuth credential) and at least one input and one output
	// format must be declared, or the host treats the executor as unavailable.
	Executor              bool     `json:"executor"`
	ExecutorModelScope    string   `json:"executor_model_scope"`
	ExecutorInputFormats  []string `json:"executor_input_formats"`
	ExecutorOutputFormats []string `json:"executor_output_formats"`
	UsagePlugin           bool     `json:"usage_plugin"`
	ManagementAPI         bool     `json:"management_api"`
}

// ExecutorModelScopeStatic is pluginapi.ExecutorModelScopeStatic.
const ExecutorModelScopeStatic = "static"

// ModelRouteRequest is the part of pluginapi.ModelRouteRequest (plus the host's host_callback_id)
// the plugin reads; the headers, query, metadata and plugin descriptor are ignored.
type ModelRouteRequest struct {
	PluginID       string
	SourceFormat   string
	RequestedModel string
	Stream         bool
	Body           []byte
	HostCallbackID string `json:"host_callback_id,omitempty"`
}

// ModelRouteResponse is pluginapi.ModelRouteResponse (no json tags: capitalised keys). With
// TargetKind "self" the host fills Target in itself and ignores TargetModel.
type ModelRouteResponse struct {
	Handled     bool
	TargetKind  string
	Target      string
	TargetModel string
	Reason      string
}

// IdentifierResponse is rpcIdentifierResponse, the result of executor.identifier.
type IdentifierResponse struct {
	Identifier string `json:"identifier"`
}

// ExecutorRequest is the part of pluginapi.ExecutorRequest (plus the host's rpcExecutorRequest
// stream_id and host_callback_id) the plugin reads; no json tags: capitalised keys. Headers and Query
// are http.Header and url.Values on the host, both map[string][]string on the wire.
type ExecutorRequest struct {
	Model           string
	Format          string
	Stream          bool
	Alt             string
	Headers         map[string][]string
	Query           map[string][]string
	OriginalRequest []byte
	SourceFormat    string
	Payload         []byte
	StreamID        string `json:"stream_id,omitempty"`
	HostCallbackID  string `json:"host_callback_id,omitempty"`
}

// ExecutorResponse is pluginapi.ExecutorResponse (no json tags: capitalised keys); Payload is base64.
type ExecutorResponse struct {
	Payload []byte
	Headers map[string][]string
}

// HostModelRequest is pluginapi.HostModelExecutionRequest plus the host_callback_id that ties the
// callback to the request being served (snake_case tags, Body is base64).
type HostModelRequest struct {
	EntryProtocol  string              `json:"entry_protocol"`
	ExitProtocol   string              `json:"exit_protocol"`
	Model          string              `json:"model"`
	Stream         bool                `json:"stream"`
	Body           []byte              `json:"body"`
	Headers        map[string][]string `json:"headers"`
	Query          map[string][]string `json:"query"`
	Alt            string              `json:"alt"`
	HostCallbackID string              `json:"host_callback_id,omitempty"`
}

// ExecutorStreamResponse is rpcExecutorStreamResponse, the result of executor.execute_stream: only the
// headers travel here, the chunks follow asynchronously through host.stream.emit / host.stream.close
// addressed by the stream_id of the request.
type ExecutorStreamResponse struct {
	Headers map[string][]string `json:"headers,omitempty"`
}

// HostModelStreamResponse is pluginapi.HostModelStreamResponse, the result of host.model.execute_stream.
type HostModelStreamResponse struct {
	StatusCode int                 `json:"status_code"`
	Headers    map[string][]string `json:"headers"`
	StreamID   string              `json:"stream_id"`
}

// HostModelStreamRef is pluginapi.HostModelStreamReadRequest and HostModelStreamCloseRequest.
type HostModelStreamRef struct {
	StreamID string `json:"stream_id"`
}

// HostModelStreamChunk is pluginapi.HostModelStreamReadResponse: one chunk, or the terminal state
// (Done, with Error set when the stream failed). Payload is base64.
type HostModelStreamChunk struct {
	Payload []byte `json:"payload"`
	Error   string `json:"error"`
	Done    bool   `json:"done"`
}

// StreamEmitRequest is rpcStreamEmitRequest (host.stream.emit): one chunk for the stream the host opened
// for executor.execute_stream. Payload is base64.
type StreamEmitRequest struct {
	StreamID string `json:"stream_id"`
	Payload  []byte `json:"payload,omitempty"`
}

// StreamCloseRequest is rpcStreamCloseRequest (host.stream.close): the end of the stream, with the
// structured error when it failed (the host surfaces its status and message to the client).
type StreamCloseRequest struct {
	StreamID    string `json:"stream_id"`
	Error       string `json:"error,omitempty"`
	ErrorDetail *Error `json:"error_detail,omitempty"`
}

// HostModelResponse is pluginapi.HostModelExecutionResponse (snake_case tags, Body is base64).
type HostModelResponse struct {
	StatusCode int                 `json:"status_code"`
	Headers    map[string][]string `json:"headers"`
	Body       []byte              `json:"body"`
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

// ErrorEnvelopeStatus builds a failed envelope that carries the HTTP status the host surfaces to the
// client (the host defaults to 500 when it is 0) and an optional retry hint.
func ErrorEnvelopeStatus(code, message string, status, retryAfterSeconds int) []byte {
	raw, err := Marshal(Envelope{OK: false, Error: &Error{
		Code: code, Message: message, HTTPStatus: status, RetryAfterSeconds: retryAfterSeconds,
		Retryable: status == 429 || status == 502 || status == 503 || status == 504,
	}})
	if err != nil {
		return []byte(`{"ok":false,"error":{"code":"internal_error","message":"encode failure"}}`)
	}
	return raw
}

// ErrorEnvelope builds a failed envelope. It never fails: the inputs are plain strings.
func ErrorEnvelope(code, message string) []byte {
	raw, err := Marshal(Envelope{OK: false, Error: &Error{Code: code, Message: message}})
	if err != nil {
		return []byte(`{"ok":false,"error":{"code":"internal_error","message":"encode failure"}}`)
	}
	return raw
}
