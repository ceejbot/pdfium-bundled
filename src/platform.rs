// Shared platform + version data for the library and the build script.
//
// `build.rs` is compiled before the crate it builds, so it can't `use` any of
// this crate's items. `include!("src/platform.rs")` textually pastes this file
// into both `build.rs` and `src/lib.rs` (the latter via `mod platform;`),
// making it the single source of truth for the pinned pdfium build and the
// per-platform archive layout — the two facts that previously lived, verbatim
// and un-enforced, in both files.
//
// Constraints that follow from being included two ways:
//   - No `//!` header (illegal when pasted mid-`build.rs`); use `//` and `///`.
//   - Nothing from `lib.rs` (the `Error` enum, its imports). `platform_for`
//     returns `Option` so each caller builds its own error type.

/// The `bblanchon/pdfium-binaries` release tag to download
/// ([`chromium/8066`](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8066)).
///
/// Must be at least [`PDFIUM_API_FLOOR`], the build that `pdfium-render`'s
/// `pdfium_latest` feature binds. Newer builds work because pdfium's C API only
/// grows: `pdfium-render` resolves every symbol eagerly at `bind()`, so a build
/// that dropped one would fail there with a missing-symbol error, not at
/// compile time. An older build fails the same way. `just smoke` and the CI
/// `bind` job exercise a real `bind()` on every change for exactly this
/// reason. Defined here once so the runtime download and the compile-time
/// embed can never disagree.
pub const PDFIUM_VERSION: &str = "8066";

/// The pdfium build `pdfium-render`'s `pdfium_latest` feature binds against
/// (`pdfium_latest = ["pdfium_7881"]` in pdfium-render 0.9.x). Update this when
/// bumping `pdfium-render` moves `pdfium_latest`; [`PDFIUM_VERSION`] must never
/// fall below it.
pub const PDFIUM_API_FLOOR: &str = "7881";

/// Base URL for `bblanchon/pdfium-binaries` release assets.
pub(crate) const BASE_URL: &str = "https://github.com/bblanchon/pdfium-binaries/releases/download";

/// Where to find the pdfium shared library for one target platform.
pub(crate) struct PlatformInfo {
    /// Asset filename in the GitHub release, e.g. `pdfium-mac-arm64.tgz`.
    pub(crate) archive_name: &'static str,
    /// Relative path inside the archive, e.g. `lib/libpdfium.dylib`.
    pub(crate) lib_path_in_archive: &'static str,
    /// Filename to write on disk, e.g. `libpdfium.dylib`.
    pub(crate) lib_name: &'static str,
}

/// Maps a Rust `(os, arch)` pair — `std::env::consts` values at runtime, or the
/// `CARGO_CFG_TARGET_*` build vars at compile time — to its pdfium archive, or
/// `None` when the platform is unsupported. The caller owns the failure: it
/// already holds the pair and builds whatever error it needs.
pub(crate) fn platform_for(os: &str, arch: &str) -> Option<PlatformInfo> {
    let info = match (os, arch) {
        ("macos", "aarch64") => PlatformInfo {
            archive_name: "pdfium-mac-arm64.tgz",
            lib_path_in_archive: "lib/libpdfium.dylib",
            lib_name: "libpdfium.dylib",
        },
        ("macos", "x86_64") => PlatformInfo {
            archive_name: "pdfium-mac-x64.tgz",
            lib_path_in_archive: "lib/libpdfium.dylib",
            lib_name: "libpdfium.dylib",
        },
        ("linux", "x86_64") => PlatformInfo {
            archive_name: "pdfium-linux-x64.tgz",
            lib_path_in_archive: "lib/libpdfium.so",
            lib_name: "libpdfium.so",
        },
        ("linux", "aarch64") => PlatformInfo {
            archive_name: "pdfium-linux-arm64.tgz",
            lib_path_in_archive: "lib/libpdfium.so",
            lib_name: "libpdfium.so",
        },
        ("windows", "x86_64") => PlatformInfo {
            archive_name: "pdfium-win-x64.tgz",
            lib_path_in_archive: "bin/pdfium.dll",
            lib_name: "pdfium.dll",
        },
        ("windows", "aarch64") => PlatformInfo {
            archive_name: "pdfium-win-arm64.tgz",
            lib_path_in_archive: "bin/pdfium.dll",
            lib_name: "pdfium.dll",
        },
        ("windows", "x86") => PlatformInfo {
            archive_name: "pdfium-win-x86.tgz",
            lib_path_in_archive: "bin/pdfium.dll",
            lib_name: "pdfium.dll",
        },
        _ => return None,
    };
    Some(info)
}
