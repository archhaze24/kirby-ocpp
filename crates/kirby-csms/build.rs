//! Embeds the built web UI. Every file under `web/dist/` (the output of `trunk build --release`
//! in `crates/kirby-csms-web`) goes into a table of served path and bytes that `src/api/web.rs`
//! includes. Without a built `dist/` the table is empty and the binary serves a short page
//! that names the build command, so a plain `cargo build` needs no wasm tools.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

fn main() -> io::Result<()> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap_or_default());
    let web = manifest.join("web");
    let dist = web.join("dist");

    println!("cargo:rerun-if-changed=build.rs");
    // A missing path counts as changed on every build, so watch `web/` (it always exists in a
    // checkout: it holds the ignore file for `dist/`) and fall back to the script itself.
    if web.is_dir() {
        println!("cargo:rerun-if-changed=web");
    }

    let mut files = Vec::new();
    if dist.is_dir() {
        collect(&dist, &dist, &mut files)?;
    }
    files.sort();

    let mut table = String::from("&[\n");
    for (served, file) in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        let file = file.display().to_string();
        writeln!(table, "    ({served:?}, include_bytes!({file:?})),").map_err(io::Error::other)?;
    }
    table.push_str("]\n");
    fs::write(out_dir.join("web_assets.rs"), table)
}

/// Adds every file below `dir` as (`/relative/path`, absolute path). Paths under `/v1` would
/// shadow the API and are skipped.
fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // Trunk leaves a lock file and a staging directory behind.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files)?;
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<_> = relative.iter().map(|part| part.to_string_lossy()).collect();
        let served = format!("/{}", parts.join("/"));
        if served == "/v1" || served.starts_with("/v1/") {
            continue;
        }
        files.push((served, path));
    }
    Ok(())
}
