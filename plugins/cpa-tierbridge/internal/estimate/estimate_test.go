package estimate

import (
	"strings"
	"testing"
)

func TestFromBody(t *testing.T) {
	bigData := strings.Repeat("A", 100000) // an image payload must not count as prompt text
	tests := []struct {
		name      string
		body      string
		wantBytes int
		wantTools bool
		wantImage bool
		wantParse bool
	}{
		{
			name:      "empty body",
			body:      ``,
			wantParse: false,
		},
		{
			name:      "chat string content",
			body:      `{"model":"x","messages":[{"role":"system","content":"abcd"},{"role":"user","content":"efghij"}]}`,
			wantBytes: 10,
			wantParse: true,
		},
		{
			name:      "chat parts with image_url",
			body:      `{"messages":[{"role":"user","content":[{"type":"text","text":"look"},{"type":"image_url","image_url":{"url":"data:image/png;base64,` + bigData + `"}}]}]}`,
			wantBytes: 4,
			wantImage: true,
			wantParse: true,
		},
		{
			name:      "chat tool calls and tool result",
			body:      `{"messages":[{"role":"assistant","content":null,"tool_calls":[{"id":"1","type":"function","function":{"name":"f","arguments":"{\"a\":1}"}}]},{"role":"tool","content":"ok"}]}`,
			wantBytes: 7 + 2,
			wantParse: true,
		},
		{
			name:      "responses string input and instructions",
			body:      `{"model":"x","instructions":"be brief","input":"hello"}`,
			wantBytes: 8 + 5,
			wantParse: true,
		},
		{
			name:      "responses item input with input_image",
			body:      `{"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"},{"type":"input_image","image_url":"data:image/png;base64,` + bigData + `"}]},{"type":"function_call_output","call_id":"c","output":"done"}]}`,
			wantBytes: 2 + 4,
			wantImage: true,
			wantParse: true,
		},
		{
			name:      "claude system string, image block and tools",
			body:      `{"system":"sys","messages":[{"role":"user","content":[{"type":"text","text":"hey"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"` + bigData + `"}}]}],"tools":[{"name":"t","input_schema":{"type":"object"}}]}`,
			wantBytes: 3 + 3 + len(`[{"input_schema":{"type":"object"},"name":"t"}]`),
			wantTools: true,
			wantImage: true,
			wantParse: true,
		},
		{
			name:      "claude system blocks and tool_use input",
			body:      `{"system":[{"type":"text","text":"abcd"}],"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"1","name":"f","input":{"q":"xy"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"1","content":"res"}]}]}`,
			wantBytes: 4 + len(`{"q":"xy"}`) + 3,
			wantParse: true,
		},
		{
			name:      "empty tools array is not tools",
			body:      `{"messages":[{"role":"user","content":"abcd"}],"tools":[]}`,
			wantBytes: 4,
			wantParse: true,
		},
		{
			name:      "legacy functions count as tools",
			body:      `{"messages":[{"role":"user","content":"abcd"}],"functions":[{"name":"f"}]}`,
			wantBytes: 4 + len(`[{"name":"f"}]`),
			wantTools: true,
			wantParse: true,
		},
		{
			name:      "multibyte text counts bytes",
			body:      `{"messages":[{"role":"user","content":"ñññ"}]}`,
			wantBytes: 6,
			wantParse: true,
		},
		{
			name:      "not json falls back to size",
			body:      `hello there`,
			wantBytes: len(`hello there`),
			wantParse: false,
		},
		{
			name:      "json that is not an object falls back to size",
			body:      `["a","b"]`,
			wantBytes: len(`["a","b"]`),
			wantParse: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := FromBody([]byte(tc.body))
			if got.Bytes != tc.wantBytes {
				t.Errorf("Bytes = %d, want %d", got.Bytes, tc.wantBytes)
			}
			if want := (tc.wantBytes + 3) / 4; got.Tokens != want {
				t.Errorf("Tokens = %d, want %d", got.Tokens, want)
			}
			if got.Tools != tc.wantTools {
				t.Errorf("Tools = %v, want %v", got.Tools, tc.wantTools)
			}
			if got.Image != tc.wantImage {
				t.Errorf("Image = %v, want %v", got.Image, tc.wantImage)
			}
			if got.Parsed != tc.wantParse {
				t.Errorf("Parsed = %v, want %v", got.Parsed, tc.wantParse)
			}
		})
	}
}
