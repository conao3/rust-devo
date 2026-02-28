ROOT := ..
CONFIG := ../examples/devo.toml

.PHONY: help
help:
	@printf '%s\n' 'targets: build check plan run fmt'

.PHONY: build
build:
	cd $(ROOT) && cargo build

.PHONY: check
check:
	cd $(ROOT) && cargo check

.PHONY: plan
plan:
	cd $(ROOT) && cargo run -- plan -f $(CONFIG)

.PHONY: run
run:
	cd $(ROOT) && cargo run -- run -f $(CONFIG) --print-script

.PHONY: fmt
fmt:
	cd $(ROOT) && nix fmt
