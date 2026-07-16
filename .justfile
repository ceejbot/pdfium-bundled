# list recipes
_help:
	just -l

# Install tools.
setup:
	brew tap ceejbot/tap
	brew install cargo-audit cargo-nextest cargo-update tomato semver-bump

# Run all tests using nextest, across the workspace.
@test:
	cargo nextest run --all-targets --no-tests=pass

# Run the CI checks (default features; see `check-bundled` for the bundled one).
@ci: test
	cargo test --doc
	cargo clippy --all-targets -- -D warnings
	cargo audit
	cargo +nightly fmt --check --all

# Lint the `bundled` feature. Downloads PDFium at build time, so needs network.
@check-bundled:
	cargo clippy --all-targets --features bundled -- -D warnings

# Bind PDFium end-to-end in both modes (the drift-catcher CI runs). Needs network.
@smoke:
	cargo run --example smoke
	cargo run --example smoke --features bundled

# Format the source.
@fmt:
	cargo +nightly fmt --all
