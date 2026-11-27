# cpa-tierbridge: CLIProxyAPI native plugin (c-shared library).
#
# Host pin: CLIProxyAPI 47f18e4d, native ABI version 1, RPC schema version 10. The native ABI and the
# schema change often upstream: rebuild and re-verify against every host version you deploy.
#
# `make dylib` is the exact build the CLIProxyAPI examples use for macOS/arm64 (examples/plugin/Makefile:
# GOARM64=v8.0, CGO_CFLAGS=-march=armv8-a, so the library loads in the host build whatever the CPU).
# dist/ is git-ignored: a built library is never committed.

VERSION ?= 0.1.0
DIST    := dist
DYLIB   := $(DIST)/tierbridge-v$(VERSION).dylib

.PHONY: all check test dylib clean

all: check dylib

# The increment's acceptance gate: vet, gofmt and the unit tests.
check:
	go vet ./...
	@test -z "$$(gofmt -l .)" || { echo "gofmt needed:"; gofmt -l .; exit 1; }
	go test ./...

test:
	go test ./...

dylib:
	mkdir -p $(DIST)
	GOARCH=arm64 GOARM64=v8.0 CGO_ENABLED=1 CGO_CFLAGS="-march=armv8-a" go build -buildmode=c-shared -o $(DYLIB) .
	rm -f $(DIST)/tierbridge-v$(VERSION).h

clean:
	rm -rf $(DIST)
