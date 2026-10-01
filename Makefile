.PHONY: check build run

check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	cargo test --workspace --locked

build:
	cargo build --workspace --locked

run:
	cargo run --locked -p bitchat-linux
