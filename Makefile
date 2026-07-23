.PHONY: build test clippy fmt run check dist install clean

build:
	cargo build --workspace

test:
	cargo test --workspace

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

check: fmt clippy test

run:
	cargo run -p mimi-app

# macOS app bundle (dist/Mimi.app). Must run on macOS.
dist:
	./scripts/package-macos.sh

# macOS universal (arm64 + x86_64) bundle.
dist-universal:
	./scripts/package-macos.sh --universal

install: dist
	cp -R dist/Mimi.app /Applications/

clean:
	cargo clean
	rm -rf dist
