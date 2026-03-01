.PHONY: help
help:
	@printf '%s\n' 'targets: build check plan run fmt'

.PHONY: build
build:
	cargo build

.PHONY: check
check:
	cargo check

.PHONY: plan
plan:
	cargo run -- plan -f examples/devo.yaml

.PHONY: run
run:
	cargo run -- run -f examples/devo.yaml --print-script

.PHONY: fmt
fmt:
	nix fmt
