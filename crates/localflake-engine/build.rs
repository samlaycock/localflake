//! Embeds DuckDB's ICU extension (time zone support) in the engine, so
//! localflake never needs the network at runtime.
//!
//! The extension comes from, in order:
//! 1. `LOCALFLAKE_ICU_EXTENSION`: a path to an `icu.duckdb_extension[.gz]` file
//!    (for offline builds);
//! 2. DuckDB's own cache, `~/.duckdb/extensions/<version>/<platform>/`;
//! 3. a download from extensions.duckdb.org (with `curl`).
//!
//! If none works the build still succeeds, with a warning; the engine then
//! falls back to installing ICU at runtime.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The DuckDB version bundled by the `duckdb` crate. Keep in step with it:
/// DuckDB only loads extensions built for its own version.
const DUCKDB_VERSION: &str = "v2.0.0";

fn main() {
    println!("cargo:rerun-if-env-changed=LOCALFLAKE_ICU_EXTENSION");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let target = out.join("icu.duckdb_extension.gz");
    let platform = platform();
    let embedded = match platform.as_deref().map(|p| obtain(p, &target)) {
        Some(Ok(())) => true,
        Some(Err(err)) => {
            println!("cargo:warning=ICU extension not embedded ({err}); localflake will install it at runtime");
            false
        }
        None => {
            println!("cargo:warning=no prebuilt ICU extension for this target; localflake will install it at runtime");
            false
        }
    };
    let source = if embedded {
        format!("pub const ICU_EXTENSION: Option<&[u8]> = Some(include_bytes!({:?}));", target.display().to_string())
    } else {
        "pub const ICU_EXTENSION: Option<&[u8]> = None;".to_owned()
    };
    let source = format!("pub const DUCKDB_VERSION: &str = {DUCKDB_VERSION:?};\n{source}\n");
    std::fs::write(out.join("icu_extension.rs"), source).unwrap();
}

/// DuckDB's name for the target platform.
fn platform() -> Option<String> {
    let os = std::env::var("CARGO_CFG_TARGET_OS").ok()?;
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").ok()?;
    let os = match os.as_str() {
        "macos" => "osx",
        "linux" => "linux",
        "windows" => "windows",
        _ => return None,
    };
    let arch = match arch.as_str() {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        _ => return None,
    };
    Some(format!("{os}_{arch}"))
}

/// Writes the gzipped extension to `target`.
fn obtain(platform: &str, target: &Path) -> Result<(), String> {
    if let Ok(path) = std::env::var("LOCALFLAKE_ICU_EXTENSION") {
        println!("cargo:rerun-if-changed={path}");
        return copy_gzipped(Path::new(&path), target);
    }
    if target.exists() {
        return Ok(());
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let cached =
            Path::new(&home).join(format!(".duckdb/extensions/{DUCKDB_VERSION}/{platform}/icu.duckdb_extension"));
        if cached.exists() {
            return copy_gzipped(&cached, target);
        }
    }
    let url = format!("http://extensions.duckdb.org/{DUCKDB_VERSION}/{platform}/icu.duckdb_extension.gz");
    let partial = target.with_extension("part");
    let status = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--output"])
        .arg(&partial)
        .arg(&url)
        .status()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !status.success() {
        return Err(format!("download of {url} failed"));
    }
    std::fs::rename(&partial, target).map_err(|e| e.to_string())
}

/// Copies an extension to `target`, gzipping it unless it already is.
fn copy_gzipped(source: &Path, target: &Path) -> Result<(), String> {
    let bytes = std::fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return std::fs::write(target, bytes).map_err(|e| e.to_string());
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&bytes).map_err(|e| e.to_string())?;
    std::fs::write(target, encoder.finish().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
