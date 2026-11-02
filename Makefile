.PHONY: help
help:
	@printf '%s\n' 'targets: build check test plan run fmt'

.PHONY: build
build:
	cargo build

.PHONY: check
check:
	cargo check

.PHONY: test
test:
	cargo test

.PHONY: plan
plan:
	cargo run -- session generate-session-script --file examples/devo.yaml --output text

.PHONY: run
run:
	cargo run -- session start-session --file examples/devo.yaml --session-name devo-example

.PHONY: fmt
fmt:
	nix fmt
