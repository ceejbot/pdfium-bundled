//! # pdfium-bundled
//!
//! Auto-download and cache [PDFium](https://pdfium.googlesource.com/pdfium/)
//! binaries at runtime, so that users of `pdfium-render` no longer need to
//! manually download libpdfium and set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`.
//!
//! ## How it works
//!
//! On first call to [`bind_pdfium`] or [`ensure_pdfium_library`]:
//!
//! 1. Checks `~/.cache/pdfium-bundled/pdfium-{VERSION}/` for the platform
//!    library.
//! 2. If absent, downloads the correct `.tgz` from [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries).
//! 3. Extracts `lib/libpdfium.dylib` (or `.so` / `.dll`) to the cache dir.
//! 4. Calls [`Pdfium::bind_to_library`] to load the real library.
//!
//! Subsequent calls skip the network entirely — the library is already cached.
//!
//! ## `bundled` feature — compile-time embedding
//!
//! For use-cases that require a fully self-contained binary (e.g., CI/CD
//! distribution), the optional `bundled` feature embeds the pdfium shared
//! library directly into the compiled executable.
//!
//! **Build steps:**
//!
//! ```sh
//! # 1. Download and extract the platform archive (example: macOS arm64).
//! curl -L https://github.com/bblanchon/pdfium-binaries/releases/download/ \
//!      chromium%2F7881/pdfium-mac-arm64.tgz | tar xz
//!
//! # 2. Build with the bundled feature, pointing PDFIUM_BUNDLE_LIB at the lib.
//! PDFIUM_BUNDLE_LIB=./lib/libpdfium.dylib \
//!   cargo build --release --features pdfium-bundled/bundled
//! ```
//!
//! At runtime, the embedded bytes are extracted to the cache directory on
//! first use ([`ensure_pdfium_bundled`] / [`bind_bundled`]).  The resulting
//! binary ships without any external dependency on libpdfium or network access.
//!
//! **Trade-offs:**
//!
//! | | Runtime-download (`bind_pdfium`) | Compile-time-bundled (`bind_bundled`) |
//! |--|--|--|
//! | Binary size | ~5 MB | ~35 MB (+30 MB) |
//! | First run | Downloads pdfium (~20 s) | Instant (already embedded) |
//! | Net access required at runtime | Once (first run) | Never |
//! | Net access required at compile time | No | No |
//! | Cross-platform binary | N/A (same arch) | Same constraints |
//!
//! ## Usage
//!
//! ```rust,no_run
//! use pdfium_bundled::{bind_pdfium_silent, bind_pdfium_from_path, ensure_pdfium_library};
//!
//! // Option A: convenient one-shot bind (silent, no progress)
//! let pdfium = bind_pdfium_silent().expect("PDFium unavailable");
//!
//! // Option B: download with progress, then bind
//! let path = ensure_pdfium_library(Some(&|downloaded, total| {
//!     if let Some(t) = total {
//!         eprint!("\rDownloading PDFium: {}/{} bytes", downloaded, t);
//!     }
//! })).expect("download failed");
//! let pdfium = bind_pdfium_from_path(&path).expect("bind failed");
//! ```
//!
//! ## Platform support
//!
//! | OS      | Arch    | Library               |
//! |---------|---------|-----------------------|
//! | macOS   | arm64   | `libpdfium.dylib`     |
//! | macOS   | x86_64  | `libpdfium.dylib`     |
//! | Linux   | x86_64  | `libpdfium.so`        |
//! | Linux   | aarch64 | `libpdfium.so`        |
//! | Windows | x86_64  | `pdfium.dll`          |
//! | Windows | aarch64 | `pdfium.dll`          |
//! | Windows | x86     | `pdfium.dll`          |
//!
//! ## Environment variable overrides
//!
//! - `PDFIUM_LIB_PATH` — path to an existing pdfium library; skips download.
//! - `PDFIUM_BUNDLED_CACHE_DIR` — override the default (runtime) cache
//!   directory.
//! - `PDFIUM_NO_AUTO_DOWNLOAD` — never hit the network; error unless the
//!   library is already cached (for CI).
//! - `PDFIUM_BUNDLE_LIB` — (compile time) path to the dylib to embed when the
//!   `bundled` feature is active.
//! - `PDFIUM_BUILD_CACHE_DIR` — (compile time) override the build-time download
//!   cache.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use etcetera::base_strategy::{BaseStrategy, Xdg};
/// Re-export of the [`pdfium-render`](https://docs.rs/pdfium-render) crate that
/// backs this one. Callers use the returned [`Pdfium`] — and the rest of the
/// PDF API — through this re-export, so they never have to add (and
/// version-match) a separate `pdfium-render` dependency:
///
/// ```no_run
/// use pdfium_bundled::pdfium_render::prelude::*;
/// ```
pub use pdfium_render;
use pdfium_render::prelude::Pdfium;
use thiserror::Error;

mod platform;

pub use crate::platform::PDFIUM_VERSION;
use crate::platform::{BASE_URL, PlatformInfo, platform_for};

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors returned by pdfium-bundled operations.
#[derive(Error, Debug)]
pub enum Error {
    #[error("Unsupported platform: {os}/{arch}")]
    UnsupportedPlatform { os: String, arch: String },

    #[error("Cache directory error: {0}")]
    CacheDir(#[source] std::io::Error),

    #[error("Download failed: {0}")]
    Download(String),

    #[error("Archive extraction failed: {0}")]
    Extract(String),

    /// `pdfium-render` loaded the file but could not resolve its symbols —
    /// most often a [`PDFIUM_VERSION`] / `pdfium_latest` mismatch, not a
    /// corrupt library.
    #[error("Failed to bind PDFium from '{path}': {reason}")]
    Bind { path: PathBuf, reason: String },
}

// ── Internal: platform metadata ──────────────────────────────────────────────

/// Resolves the current host's pdfium archive, mapping an unsupported platform
/// to [`Error::UnsupportedPlatform`]. The lookup table lives in `platform.rs`,
/// shared verbatim with `build.rs` so the two can't drift.
fn detect_platform() -> Result<PlatformInfo, Error> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    platform_for(os, arch).ok_or_else(|| Error::UnsupportedPlatform {
        os: os.to_string(),
        arch: arch.to_string(),
    })
}

// ── Cache directory resolution ───────────────────────────────────────────────

/// Returns the per-version cache directory for the PDFium library.
///
/// Uses the [XDG base-directory strategy][xdg] on **every** platform — macOS
/// and Windows included — so the cache honors `$XDG_CACHE_HOME` when set and
/// otherwise falls back to `~/.cache`, rather than `~/Library/Caches` (macOS)
/// or `%LOCALAPPDATA%` (Windows). This is the layout developers usually expect
/// from a cross-platform CLI tool. The resulting path is:
///
/// - `$XDG_CACHE_HOME/pdfium-bundled/pdfium-{VERSION}/`, or
/// - `~/.cache/pdfium-bundled/pdfium-{VERSION}/` when `XDG_CACHE_HOME` is
///   unset.
///
/// Override the whole path by setting `PDFIUM_BUNDLED_CACHE_DIR`.
///
/// [xdg]: https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html
#[must_use]
pub fn pdfium_cache_dir() -> PathBuf {
    let override_dir = std::env::var_os("PDFIUM_BUNDLED_CACHE_DIR").map(PathBuf::from);

    // Fall back to a temp dir only if the home directory can't be determined.
    let base = Xdg::new()
        .map(|xdg| xdg.cache_dir())
        .unwrap_or_else(|_| std::env::temp_dir());

    resolve_cache_dir(override_dir, base)
}

/// Assembles the per-version cache directory from an optional override root and
/// the default base cache directory.
///
/// Split out from [`pdfium_cache_dir`] as a pure function (no environment or
/// filesystem access) so the layout logic can be unit-tested without mutating
/// process-global state — `std::env::set_var` is `unsafe` under the 2024
/// edition precisely because it races with concurrent readers.
fn resolve_cache_dir(override_dir: Option<PathBuf>, base: PathBuf) -> PathBuf {
    let root = override_dir.unwrap_or_else(|| base.join("pdfium-bundled"));
    root.join(format!("pdfium-{PDFIUM_VERSION}"))
}

// ── Thread-safe singleton path cache ─────────────────────────────────────────

static RESOLVED_PATH: OnceLock<PathBuf> = OnceLock::new();

// ── Public API ───────────────────────────────────────────────────────────────

/// Returns `true` if the PDFium library is already cached on disk (no network
/// access needed on next call to [`ensure_pdfium_library`]).
///
/// Also returns `true` when `PDFIUM_LIB_PATH` points to an existing file.
#[must_use]
pub fn is_pdfium_cached() -> bool {
    cached_pdfium_path().is_some()
}

/// Returns the on-disk path to the PDFium library, or `None` if not cached.
#[must_use]
pub fn cached_pdfium_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PDFIUM_LIB_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    if let Ok(info) = detect_platform() {
        let p = pdfium_cache_dir().join(info.lib_name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Ensures the PDFium dynamic library is present in the local cache.
///
/// - If `PDFIUM_LIB_PATH` is set (and the file exists), that path is used.
/// - Otherwise, checks `pdfium_cache_dir()` for an existing library.
/// - If absent, downloads the appropriate platform binary from GitHub and
///   extracts it to the cache directory.
///
/// `on_progress` receives `(bytes_downloaded, total_size_option)` during
/// the download.  Pass `None` to suppress progress callbacks.
///
/// # Thread safety
///
/// Safe to call from multiple threads simultaneously; the download happens
/// only once per process lifetime.
pub fn ensure_pdfium_library(on_progress: Option<&dyn Fn(u64, Option<u64>)>) -> Result<PathBuf, Error> {
    // Fast path: already resolved in this process.
    if let Some(path) = RESOLVED_PATH.get() {
        return Ok(path.clone());
    }

    let path = resolve_or_download(on_progress)?;

    // Best-effort cache in the OnceLock (ignore race; both will succeed).
    let _ = RESOLVED_PATH.set(path.clone());

    Ok(path)
}

/// Binds to PDFium, downloading it first if necessary.
///
/// `on_progress` receives `(bytes_downloaded, total_bytes_option)` during
/// the initial download.
pub fn bind_pdfium(on_progress: Option<&dyn Fn(u64, Option<u64>)>) -> Result<Pdfium, Error> {
    let lib_path = ensure_pdfium_library(on_progress)?;
    bind_pdfium_from_path(&lib_path)
}

/// Binds to PDFium without any progress output.
///
/// Downloads and caches on first call if required.
pub fn bind_pdfium_silent() -> Result<Pdfium, Error> {
    bind_pdfium(None)
}

/// Binds to a PDFium library at an explicit `path`.
///
/// Does not interact with the download / cache layer.
pub fn bind_pdfium_from_path(path: &Path) -> Result<Pdfium, Error> {
    Pdfium::bind_to_library(path).map(Pdfium::new).map_err(|e| Error::Bind {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

// ── Bundled feature ──────────────────────────────────────────────────────────
//
// With `--features bundled`, build.rs embeds the pdfium bytes into the binary
// via `include_bytes!`; on first use they're written to the cache dir and
// loaded from there. The crate-level docs cover the build workflow.

#[cfg(feature = "bundled")]
mod bundled_lib {
    // `bundled.rs` is generated by build.rs and defines:
    //   pub static PDFIUM_BYTES: &[u8] = include_bytes!("bundled_pdfium_lib");
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
}

/// Ensures the embedded PDFium library is extracted to the local cache and
/// returns its on-disk path.
///
/// The bytes are embedded at compile time (via `PDFIUM_BUNDLE_LIB`); on first
/// call they are written to `pdfium_cache_dir()` so the OS can load them, and
/// later calls just return the cached path.
///
/// # Errors
///
/// Returns [`Error::CacheDir`] if the cache directory cannot be created, or
/// [`Error::Extract`] if writing the library fails.
#[cfg(feature = "bundled")]
pub fn ensure_pdfium_bundled() -> Result<PathBuf, Error> {
    // Fast path: already resolved in this process.
    if let Some(path) = RESOLVED_PATH.get() {
        return Ok(path.clone());
    }

    let info = detect_platform()?;
    let cache_dir = pdfium_cache_dir();
    let lib_path = cache_dir.join(info.lib_name);

    if !lib_path.exists() {
        std::fs::create_dir_all(&cache_dir).map_err(Error::CacheDir)?;
        std::fs::write(&lib_path, bundled_lib::PDFIUM_BYTES).map_err(|e| {
            Error::Extract(format!(
                "Failed to write bundled pdfium to {}: {}",
                lib_path.display(),
                e
            ))
        })?;

        // On Unix, ensure the shared library is executable so the dynamic
        // linker accepts it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&lib_path).map_err(Error::CacheDir)?.permissions();
            perms.set_mode(perms.mode() | 0o755);
            std::fs::set_permissions(&lib_path, perms).map_err(Error::CacheDir)?;
        }
    }

    let _ = RESOLVED_PATH.set(lib_path.clone());
    Ok(lib_path)
}

/// Binds to the PDFium library that was embedded at compile time.
///
/// Extracts the library to the local cache directory on first call (see
/// [`ensure_pdfium_bundled`]). No network access is required.
#[cfg(feature = "bundled")]
pub fn bind_bundled() -> Result<Pdfium, Error> {
    let lib_path = ensure_pdfium_bundled()?;
    bind_pdfium_from_path(&lib_path)
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn resolve_or_download(on_progress: Option<&dyn Fn(u64, Option<u64>)>) -> Result<PathBuf, Error> {
    if let Ok(env_path) = std::env::var("PDFIUM_LIB_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Ok(p);
        }
        // Fall through: env var set but file missing → still auto-download.
        eprintln!(
            "pdfium-bundled: PDFIUM_LIB_PATH '{}' not found; downloading …",
            p.display()
        );
    }

    // Opt-out for CI or test stages that must never touch the network.
    if std::env::var("PDFIUM_NO_AUTO_DOWNLOAD").is_ok() {
        return Err(Error::Download(
            "auto-download disabled (PDFIUM_NO_AUTO_DOWNLOAD is set); \
             set PDFIUM_LIB_PATH to point at an existing pdfium library"
                .to_string(),
        ));
    }

    let info = detect_platform()?;
    let cache_dir = pdfium_cache_dir();
    let lib_path = cache_dir.join(info.lib_name);

    if lib_path.exists() {
        return Ok(lib_path);
    }

    let url = format!("{}/chromium%2F{}/{}", BASE_URL, PDFIUM_VERSION, info.archive_name);

    std::fs::create_dir_all(&cache_dir).map_err(Error::CacheDir)?;

    let archive_bytes = download_bytes(&url, on_progress)?;
    extract_library(&archive_bytes, info.lib_path_in_archive, &lib_path)?;

    Ok(lib_path)
}

/// Streams a URL into a `Vec<u8>`, calling `on_progress` every 64 KiB.
fn download_bytes(url: &str, on_progress: Option<&dyn Fn(u64, Option<u64>)>) -> Result<Vec<u8>, Error> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("pdfium-bundled/", env!("CARGO_PKG_VERSION")))
        .max_redirects(5)
        .build()
        .into();

    // ureq returns non-2xx as an error by default (http_status_as_error).
    let mut response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => Error::Download(format!("HTTP {code} for {url}")),
        other => Error::Download(format!("GET {url}: {other}")),
    })?;

    let total = response.body().content_length();
    let capacity = total.unwrap_or(35 * 1024 * 1024) as usize;
    let mut buf = Vec::with_capacity(capacity);

    let mut reader = response.body_mut().as_reader();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut downloaded: u64 = 0;

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                downloaded += n as u64;
                if let Some(cb) = on_progress {
                    cb(downloaded, total);
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(Error::Download(format!("Read error: {e}")));
            }
        }
    }

    Ok(buf)
}

/// Extracts a single file from a gzipped tar archive into `dest_path`.
fn extract_library(archive_bytes: &[u8], lib_path_in_archive: &str, dest_path: &Path) -> Result<(), Error> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let gz = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(gz);

    for entry in archive.entries().map_err(|e| Error::Extract(e.to_string()))? {
        let mut entry = entry.map_err(|e| Error::Extract(e.to_string()))?;
        let entry_path = entry.path().map_err(|e| Error::Extract(e.to_string()))?;

        let entry_str = entry_path.to_string_lossy();
        if entry_str == lib_path_in_archive {
            entry
                .unpack(dest_path)
                .map_err(|e| Error::Extract(format!("Unpack failed: {e}")))?;
            return Ok(());
        }
    }

    Err(Error::Extract(format!(
        "Library '{}' not found in archive",
        lib_path_in_archive
    )))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_platform_is_supported() {
        detect_platform().expect("current platform should be supported");
    }

    #[test]
    fn cache_dir_is_deterministic() {
        // pdfium_cache_dir() only *reads* the environment, so it is stable
        // across calls within a process without any env mutation.
        let d1 = pdfium_cache_dir();
        let d2 = pdfium_cache_dir();
        assert_eq!(d1, d2);
        assert!(
            d1.to_str().expect("cache dir is valid UTF-8").contains(PDFIUM_VERSION),
            "expected PDFIUM_VERSION {PDFIUM_VERSION} in {d1:?}"
        );
    }

    #[test]
    fn cache_dir_default_layout() {
        let d = resolve_cache_dir(None, PathBuf::from("/tmp/example-cache"));
        assert_eq!(
            d,
            PathBuf::from(format!("/tmp/example-cache/pdfium-bundled/pdfium-{PDFIUM_VERSION}"))
        );
    }

    #[test]
    fn cache_dir_override_takes_precedence() {
        let d = resolve_cache_dir(
            Some(PathBuf::from("/tmp/custom-override")),
            PathBuf::from("/unused-base"),
        );
        assert_eq!(
            d,
            PathBuf::from(format!("/tmp/custom-override/pdfium-{PDFIUM_VERSION}"))
        );
        assert!(
            !d.to_str().expect("cache dir is valid UTF-8").contains("unused-base"),
            "override must ignore the default base: {d:?}"
        );
    }

    #[test]
    fn platform_info_fields_nonempty() {
        let info = detect_platform().expect("current platform should be supported");
        assert!(!info.archive_name.is_empty());
        assert!(!info.lib_path_in_archive.is_empty());
        assert!(!info.lib_name.is_empty());
    }

    #[test]
    fn platform_for_maps_a_known_target() {
        let linux = platform_for("linux", "x86_64").expect("linux/x86_64 is supported");
        assert_eq!(linux.archive_name, "pdfium-linux-x64.tgz");
        assert_eq!(linux.lib_path_in_archive, "lib/libpdfium.so");
        assert_eq!(linux.lib_name, "libpdfium.so");
    }

    #[test]
    fn platform_for_rejects_unknown_targets() {
        assert!(platform_for("freebsd", "x86_64").is_none());
        assert!(platform_for("linux", "riscv64").is_none());
    }
}
