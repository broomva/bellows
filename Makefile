.PHONY: help check test clippy fmt fmt-check deny ci doc clean smoke control-audit

help:
	@echo "Bellows — Makefile targets"
	@echo "  check          — cargo check across the workspace"
	@echo "  test           — cargo test across the workspace"
	@echo "  clippy         — cargo clippy with -D warnings"
	@echo "  fmt            — cargo fmt"
	@echo "  fmt-check      — cargo fmt --check (gate)"
	@echo "  deny           — cargo deny check (license + advisories + sources)"
	@echo "  doc            — build rustdoc with --no-deps for the workspace"
	@echo "  smoke          — quick local sanity (check + test --lib)"
	@echo "  ci             — full CI suite locally (check + test + clippy + fmt-check + deny)"
	@echo "  control-audit  — alias for ci; metalayer entry point"
	@echo "  clean          — cargo clean"

check:
	cargo check --workspace --all-targets

test:
	cargo test --workspace --all-targets

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

deny:
	cargo deny check

doc:
	cargo doc --workspace --no-deps

smoke:
	cargo check --workspace
	cargo test --workspace --lib

ci: check test clippy fmt-check deny

control-audit: ci

clean:
	cargo clean
