// Command cpa-tierbridge is a CLIProxyAPI native plugin (c-shared library) that adds the
// "tierbridge/auto" model alias: a ModelRouter that ranks provider/model candidates from the router
// file exported by `tierbridge export cpa-router`, fits them to the request, and hands the pick back
// to the host's built-in auth/executor path.
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
	// The plugin makes no host callbacks, so the host API is only checked for ABI compatibility.
	if host != nil && uint32(host.abi_version) != wire.ABIVersion {
		return 1
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
			writeResponse(response, wire.ErrorEnvelope(wire.ErrorCodeInternal, fmt.Sprintf("plugin panic: %v", r)))
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
