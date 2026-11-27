// Package estimate derives what a client request needs from a model (prompt size, tools, images)
// without knowing which protocol it speaks: OpenAI chat completions, OpenAI Responses and Claude
// Messages bodies are all walked with the same rules.
package estimate

import (
	"bytes"
	"encoding/json"
)

// BytesPerToken is the rule of thumb behind the token estimate (roughly four bytes of text per token).
const BytesPerToken = 4

// Request is what a request demands from the model that will serve it.
type Request struct {
	// Tokens is the estimated prompt size: ceil(Bytes / BytesPerToken).
	Tokens int `json:"tokens"`
	// Bytes is the number of text bytes counted: message and system text, tool-call arguments and
	// results, and the tool definitions. Image payloads (URLs, base64) are not text and are skipped.
	Bytes int `json:"bytes"`
	// Tools reports that the request declares tools (a non-empty "tools" or legacy "functions").
	Tools bool `json:"tools"`
	// Image reports that a message carries an image part.
	Image bool `json:"image"`
	// Parsed is false when the body was not a JSON object; Tokens is then derived from the raw size.
	Parsed bool `json:"parsed"`
}

// textKeys are the JSON keys whose string values are prompt text in the supported protocols:
// chat/Claude "content" and "system", Responses "input"/"instructions"/"output", content parts
// "text", tool-call "arguments", and plain "prompt".
var textKeys = map[string]bool{
	"text":         true,
	"content":      true,
	"system":       true,
	"instructions": true,
	"prompt":       true,
	"arguments":    true,
	"output":       true,
	"input":        true,
}

// imageTypes are the content-part types that carry an image: chat completions "image_url",
// Responses "input_image" and Claude "image".
var imageTypes = map[string]bool{
	"image_url":   true,
	"input_image": true,
	"image":       true,
}

// imageKeys are the keys that hold an image payload (also Gemini-style inline data).
var imageKeys = map[string]bool{
	"image_url":   true,
	"input_image": true,
	"inlineData":  true,
	"inline_data": true,
}

// FromBody estimates the request body. It never fails: an unusable body yields a size-only estimate.
func FromBody(body []byte) Request {
	body = bytes.TrimSpace(body)
	if len(body) == 0 {
		return Request{}
	}
	var root any
	if err := json.Unmarshal(body, &root); err != nil {
		return rawEstimate(len(body))
	}
	obj, ok := root.(map[string]any)
	if !ok {
		return rawEstimate(len(body))
	}
	w := walker{}
	for key, val := range obj {
		switch key {
		case "tools", "functions":
			if nonEmpty(val) {
				w.tools = true
				w.bytes += jsonLen(val)
			}
		default:
			w.walk(val, key)
		}
	}
	return Request{
		Tokens: tokensFor(w.bytes),
		Bytes:  w.bytes,
		Tools:  w.tools,
		Image:  w.image,
		Parsed: true,
	}
}

func rawEstimate(n int) Request {
	return Request{Tokens: tokensFor(n), Bytes: n}
}

func tokensFor(n int) int {
	return (n + BytesPerToken - 1) / BytesPerToken
}

type walker struct {
	bytes int
	tools bool
	image bool
}

func (w *walker) walk(v any, key string) {
	switch t := v.(type) {
	case string:
		if textKeys[key] {
			w.bytes += len(t)
		}
	case []any:
		for _, e := range t {
			w.walk(e, key)
		}
	case map[string]any:
		if typ, _ := t["type"].(string); imageTypes[typ] {
			w.image = true
			return
		}
		for k, e := range t {
			switch {
			case imageKeys[k]:
				w.image = true
			case k == "input" || k == "arguments":
				// A structured tool-call input (Claude tool_use) is text the model reads as JSON.
				switch e.(type) {
				case map[string]any, []any:
					w.bytes += jsonLen(e)
				default:
					w.walk(e, k)
				}
			default:
				w.walk(e, k)
			}
		}
	}
}

func nonEmpty(v any) bool {
	switch t := v.(type) {
	case []any:
		return len(t) > 0
	case map[string]any:
		return len(t) > 0
	case nil:
		return false
	default:
		return true
	}
}

func jsonLen(v any) int {
	raw, err := json.Marshal(v)
	if err != nil {
		return 0
	}
	return len(raw)
}
