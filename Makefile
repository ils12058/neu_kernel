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
	$(PYTHON) tools/neu.py check

doctor:
	$(PYTHON) tools/neu.py doctor
