// Command cpa-tierbridge is a CLIProxyAPI native plugin (c-shared library) that adds the
// "tierbridge/auto" model alias: a ModelRouter that routes the alias to the plugin's own executor, which
// ranks the candidate models of the router file exported by `tierbridge export cpa-router`, runs the
// first that fits the request through the host (host.model.execute) and falls back down the list
// within one request.
//
// This file is only the cgo shim, modelled on CLIProxyAPI's examples/plugin/management-api/go. It
// implements the four exports of the native plugin ABI and moves bytes between C and
// internal/rpc, which holds all the logic and is tested without cgo.
//
// Host pin: CLIProxyAPI 47f18e4d, native ABI version 1, RPC schema version 10 (see internal/wire).
package main

/*
#include <stdint.h>
#include <stdlib.h>

typedef struct {
	void* ptr;
	size_t len;
} cliproxy_buffer;

typedef int (*cliproxy_host_call_fn)(void*, const char*, const uint8_t*, size_t, cliproxy_buffer*);
typedef void (*cliproxy_host_free_fn)(void*, size_t);

typedef struct {
	uint32_t abi_version;
	void* host_ctx;
	cliproxy_host_call_fn call;
	cliproxy_host_free_fn free_buffer;
} cliproxy_host_api;

// The host API is copied at every init (the host probes a library with an init+shutdown before using
// it) and used for the host.model.execute callbacks of the executor.
static cliproxy_host_api stored_host;
static int have_host;

static void store_host_api(const cliproxy_host_api* host) {
	if (host != NULL) {
		stored_host = *host;
		have_host = 1;
	}
}

static int call_host_api(const char* method, const uint8_t* request, size_t request_len, cliproxy_buffer* response) {
	if (!have_host || stored_host.call == NULL) {
		return 1;
	}
	return stored_host.call(stored_host.host_ctx, method, request, request_len, response);
}

static void free_host_buffer(void* ptr, size_t len) {
	if (have_host && stored_host.free_buffer != NULL && ptr != NULL) {
		stored_host.free_buffer(ptr, len);
	}
}

typedef int (*cliproxy_plugin_call_fn)(char*, uint8_t*, size_t, cliproxy_buffer*);
typedef void (*cliproxy_plugin_free_fn)(void*, size_t);
typedef void (*cliproxy_plugin_shutdown_fn)(void);

typedef struct {
	uint32_t abi_version;
	cliproxy_plugin_call_fn call;
	cliproxy_plugin_free_fn free_buffer;
	cliproxy_plugin_shutdown_fn shutdown;
} cliproxy_plugin_api;

extern int cliproxyPluginCall(char*, uint8_t*, size_t, cliproxy_buffer*);
extern void cliproxyPluginFree(void*, size_t);
extern void cliproxyPluginShutdown(void);
*/
import "C"

import (
	"fmt"
	"unsafe"

	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/rpc"
	"github.com/blyzer/llm-tierbridge/plugins/cpa-tierbridge/internal/wire"
)

// handler is the process-wide plugin state. It is created before any export can run and survives
// cliproxy_plugin_init/shutdown pairs (the host probes a library with an init+shutdown before using it).
var handler = rpc.NewHandler()

func main() {}

//export cliproxy_plugin_init
func cliproxy_plugin_init(host *C.cliproxy_host_api, plugin *C.cliproxy_plugin_api) (rc C.int) {
	defer func() {
		if recover() != nil {
			rc = 1
		}
	}()
	if plugin == nil {
		return 1
	}
	if host != nil && uint32(host.abi_version) != wire.ABIVersion {
		return 1
	}
	if host != nil {
		C.store_host_api(host)
		handler.SetHost(cgoHost{})
	}
	plugin.abi_version = C.uint32_t(wire.ABIVersion)
	plugin.call = C.cliproxy_plugin_call_fn(C.cliproxyPluginCall)
	plugin.free_buffer = C.cliproxy_plugin_free_fn(C.cliproxyPluginFree)
	plugin.shutdown = C.cliproxy_plugin_shutdown_fn(C.cliproxyPluginShutdown)
	return 0
}

//export cliproxyPluginCall
func cliproxyPluginCall(method *C.char, request *C.uint8_t, requestLen C.size_t, response *C.cliproxy_buffer) (rc C.int) {
	if response != nil {
		response.ptr = nil
		response.len = 0
	}
	// A Go panic crossing the cgo boundary would take the whole host down: recover everything.
	defer func() {
		if r := recover(); r != nil {
			writeResponse(response, wire.ErrorEnvelopeStatus(wire.ErrorCodeInternal, fmt.Sprintf("plugin panic: %v", r), 500, 0))
			rc = 1
		}
	}()
	if method == nil {
		writeResponse(response, wire.ErrorEnvelope("invalid_method", "method is required"))
		return 1
	}
	var payload []byte
	if request != nil && requestLen > 0 {
		// Copy: the host owns the request buffer and the handler may keep slices of it.
		payload = append([]byte(nil), unsafe.Slice((*byte)(unsafe.Pointer(request)), int(requestLen))...)
	}
	writeResponse(response, handler.Handle(C.GoString(method), payload))
	return 0
}

//export cliproxyPluginFree
func cliproxyPluginFree(ptr unsafe.Pointer, length C.size_t) {
	if ptr != nil {
		C.free(ptr)
	}
	_ = length
}

//export cliproxyPluginShutdown
func cliproxyPluginShutdown() {
	defer func() { _ = recover() }() // never let a panic cross the cgo boundary
	handler.Flush()
}

// cgoHost is the rpc.HostCaller over the host API: the only cgo the callbacks need, so everything
// around it (envelopes, deadlines, panics) is plain Go tested without cgo.
type cgoHost struct{}

func (cgoHost) Call(method string, request []byte) ([]byte, error) {
	cMethod := C.CString(method)
	defer C.free(unsafe.Pointer(cMethod))
	var response C.cliproxy_buffer
	var requestPtr *C.uint8_t
	if len(request) > 0 {
		p := C.CBytes(request)
		if p == nil {
			return nil, fmt.Errorf("allocate host callback %s", method)
		}
		defer C.free(p)
		requestPtr = (*C.uint8_t)(p)
	}
	code := C.call_host_api(cMethod, requestPtr, C.size_t(len(request)), &response)
	var raw []byte
	if response.ptr != nil && response.len > 0 {
		raw = C.GoBytes(response.ptr, C.int(response.len))
	}
	if response.ptr != nil {
		C.free_host_buffer(response.ptr, response.len)
	}
	if len(raw) == 0 {
		return nil, fmt.Errorf("host callback %s returned no response (code %d)", method, int(code))
	}
	return raw, nil
}

// writeResponse hands raw to the host in a C-allocated buffer that cliproxyPluginFree releases.
func writeResponse(response *C.cliproxy_buffer, raw []byte) {
	if response == nil || len(raw) == 0 {
		return
	}
	ptr := C.CBytes(raw)
	if ptr == nil {
		return
	}
	response.ptr = ptr
	response.len = C.size_t(len(raw))
}
