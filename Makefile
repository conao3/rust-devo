.PHONY: help
help:
	$(MAKE) -C make help

.PHONY: build
build:
	$(MAKE) -C make build

.PHONY: check
check:
	$(MAKE) -C make check

.PHONY: plan
plan:
	$(MAKE) -C make plan

.PHONY: run
run:
	$(MAKE) -C make run

.PHONY: fmt
fmt:
	$(MAKE) -C make fmt
