# Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
.PHONY: help test test_one test_all

RUST_LOG ?= zenclash_service=trace,kode_bridge=error
TEST_NAME ?=
CARGO ?= cargo
CARGO_FLAGS ?= -p zenclash-service --features standalone,client,test
TEST_BIN_ARGS ?= -- --nocapture

ifdef TARGET
CARGO_FLAGS += --target $(TARGET)
endif

ifdef NO_RUN
CARGO_FLAGS += --no-run
endif

# `test` runs one named integration test when TEST_NAME is set, otherwise the full suite.
ifdef TEST_NAME
test: test_one
else
test: test_all
endif

test_one:
	@echo "Running test: RUST_LOG=$(RUST_LOG) $(CARGO) test $(CARGO_FLAGS) --test $(TEST_NAME) $(TEST_BIN_ARGS)"
	RUST_LOG=$(RUST_LOG) $(CARGO) test $(CARGO_FLAGS) --test $(TEST_NAME) $(TEST_BIN_ARGS)

test_all:
	@echo "Running all tests: RUST_LOG=$(RUST_LOG) $(CARGO) test $(CARGO_FLAGS) $(TEST_BIN_ARGS)"
	RUST_LOG=$(RUST_LOG) $(CARGO) test $(CARGO_FLAGS) $(TEST_BIN_ARGS)

help:
	@echo "Available targets:"
	@echo "  test       - run $(if $(TEST_NAME),single test $(TEST_NAME),all tests)"
	@echo "  test_one  - run single integration test (TEST_NAME)"
	@echo "  test_all   - run all tests"
