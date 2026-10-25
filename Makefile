PYTHON ?= python3
TEST_TIMEOUT ?= 30

.PHONY: build run test check doctor

build:
	$(PYTHON) tools/neu.py build

run:
	$(PYTHON) tools/neu.py run

test:
	$(PYTHON) tools/neu.py test --timeout $(TEST_TIMEOUT)

check:
	cargo fmt --all -- --check
	cargo clippy --locked --target x86_64-unknown-none -- -D warnings
	cargo clippy --locked --target x86_64-unknown-none --features test-panic -- -D warnings
	cargo clippy --locked --target x86_64-unknown-none --features test-missing-memory-map -- -D warnings

doctor:
	$(PYTHON) tools/neu.py doctor
