# Makefile for fuse-overlayfs

CARGO ?= cargo
CARGO_HOME ?= .cargo
CARGO_FLAGS ?= --release
PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
MANDIR ?= $(PREFIX)/share/man

BINARY = target/release/fuse-overlayfs

# Integration test scripts (require root + FUSE)
INTEGRATION_TESTS = \
	tests/test-copyup.sh \
	tests/test-dir-ops.sh \
	tests/test-hardlinks.sh \
	tests/test-readonly.sh \
	tests/test-rename.sh \
	tests/test-special-files.sh \
	tests/test-symlinks.sh \
	tests/test-xattr.sh \
	tests/test-stress.sh

.PHONY: all build test unit-test integration-test install clean test-single

all: build

build:
	CARGO_HOME=$(CARGO_HOME) $(CARGO) build $(CARGO_FLAGS)

# Unit tests (no root required)
unit-test:
	CARGO_HOME=$(CARGO_HOME) $(CARGO) test $(CARGO_FLAGS)

# Integration tests (require root + FUSE)
integration-test: build
	@if [ $$(id -u) -ne 0 ]; then \
		echo "ERROR: integration tests require root"; \
		exit 1; \
	fi
	@echo "Running integration tests with $(BINARY)..."
	@failed=0; passed=0; \
	for t in $(INTEGRATION_TESTS); do \
		echo ""; \
		echo "=== Running $$t ==="; \
		if PATH="$(CURDIR)/target/release:$$PATH" bash "$$t"; then \
			echo "PASS: $$t"; \
			passed=$$((passed + 1)); \
		else \
			echo "FAIL: $$t"; \
			failed=$$((failed + 1)); \
		fi; \
	done; \
	echo ""; \
	echo "=== Results: $$passed passed, $$failed failed ==="; \
	if [ $$failed -gt 0 ]; then exit 1; fi

# Run a single integration test: make test-single TEST=tests/test-copyup.sh
test-single: build
	@if [ -z "$(TEST)" ]; then echo "Usage: make test-single TEST=tests/test-copyup.sh"; exit 1; fi
	PATH="$(CURDIR)/target/release:$$PATH" bash "$(TEST)"

# All tests
test: unit-test integration-test

install: build
	install -D -m 0755 $(BINARY) $(DESTDIR)$(BINDIR)/fuse-overlayfs

clean:
	CARGO_HOME=$(CARGO_HOME) $(CARGO) clean
