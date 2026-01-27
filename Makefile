
all: dryrun

dryrun: checks
	cargo publish --dry-run

checks: fmt clippy test audit msrv deny docs1 docs2 examples

fmt:
	cargo fmt

clippy:
	cargo clippy --all-features -- -D warnings

check:
	cargo check --all-features

test:
	cargo test --all-features

audit:
	cargo audit

msrv:
	cargo msrv verify

deny:
	cargo deny check

docs1:
	cargo doc --all-features --no-deps

docs2:
	RUSTDOCFLAGS="--cfg docsrs" cargo +nightly doc --all-features --open

SRC_EXAMPLES := $(wildcard examples/*.rs)
EXAMPLES := $(patsubst %.rs,%,$(SRC_EXAMPLES))

# test run of all examples
examples: $(EXAMPLES)

examples/%:
	cargo run --example $* --all-features
