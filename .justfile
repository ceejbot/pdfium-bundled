# list recipes
_help:
	just -l

# Install tools.
setup:
	brew tap ceejbot/tap
	brew install cargo-audit cargo-nextest cargo-update tomato semver-bump

# Run all tests using nextest, across the workspace.
@test:
	cargo nextest run --all-targets --no-tests=pass --locked

# Run the CI checks (default features; see `check-bundled` for the bundled one).
@ci: test
	cargo test --doc --locked
	cargo clippy --all-targets --locked -- -D warnings
	cargo audit
	# Docs gate: build exactly what docs.rs renders — the bundled-only API — with
	# warnings as errors, so a broken intra-doc link can't reach a release. DOCS_RS=1
	# trips build.rs's offline stub, so this never downloads pdfium (see check-bundled
	# for the network-dependent bundled lint).
	DOCS_RS=1 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features bundled --locked
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

# Regenerate the digest table in src/platform.rs for a pdfium build: download
# the seven release archives, verify each against upstream's SLSA attestation
# with `gh attestation verify`, extract, and print archive + library SHA-256s
# ready to paste. Needs network and an authenticated `gh`.
pin-digests VERSION:
	#!/usr/bin/env bash
	set -euo pipefail
	version="{{VERSION}}"
	base="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${version}"
	tmp="$(mktemp -d)"
	trap 'rm -rf "$tmp"' EXIT
	cd "$tmp"
	for name in mac-arm64 mac-x64 linux-x64 linux-arm64 win-x64 win-arm64 win-x86; do
		curl -fsSL "$base/pdfium-$name.tgz" -o "pdfium-$name.tgz"
	done
	for archive in pdfium-*.tgz; do
		gh attestation verify "$archive" --repo bblanchon/pdfium-binaries \
			--signer-workflow bblanchon/pdfium-binaries/.github/workflows/build-all.yml >/dev/null
		echo "attestation ok: $archive" >&2
	done
	sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
	echo "// chromium/${version} — verified against upstream's attestation on $(date -u +%F)"
	for archive in pdfium-*.tgz; do
		name="${archive%.tgz}"
		mkdir -p "x/$name"
		tar -xzf "$archive" -C "x/$name"
		lib="$(find "x/$name" -type f \( -name 'libpdfium.*' -o -name 'pdfium.dll' \) | head -1)"
		printf '%s\n    archive_sha256: "%s",\n    lib_sha256: "%s",\n' "$archive" "$(sha "$archive")" "$(sha "$lib")"
	done
