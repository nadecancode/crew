//! Embeds the web dashboard's built bundle (`web-dist/`) in the binary, so
//! `cargo install` needs no node (notes/web-design.md §build).
//!
//! Writes `$OUT_DIR/web_assets.rs`: one `(path, content type, bytes)` entry
//! per file, sorted by path, for `src/web/assets.rs`. Without a bundle (no
//! `web-dist/index.html`) one built-in page is embedded as `index.html` that
//! says the dashboard is not part of this build.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const BUILTIN_INDEX: &str = "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<title>crew</title>
<style>body{font:16px/1.5 system-ui,sans-serif;margin:4rem auto;max-width:36rem;padding:0 1rem}</style>
</head>
<body>
<h1>crew</h1>
<p>The web dashboard is not part of this build of crew.</p>
<p>Build it with <code>just web-build</code> and build crew again, or keep using <code>crew attach</code>.</p>
</body>
</html>
";

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Every file under `dir`, as (path relative to `root` with `/`, absolute
/// path).
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(root, &path, out);
        } else if kind.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("walked below the root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path));
        }
    }
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let dist = manifest_dir.join("web-dist");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=web-dist");

    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        walk(&dist, &dist, &mut files);
        for (_, path) in &files {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    } else {
        let builtin = out_dir.join("builtin-index.html");
        std::fs::write(&builtin, BUILTIN_INDEX).unwrap();
        files.push(("index.html".to_string(), builtin));
    }
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut code = String::from(
        "/// The embedded bundle, sorted by path: (path, content type, bytes).\n\
         pub(super) static ASSETS: &[(&str, &str, &[u8])] = &[\n",
    );
    for (rel, path) in &files {
        writeln!(
            code,
            "    ({rel:?}, {:?}, include_bytes!({:?})),",
            content_type(rel),
            path.display().to_string()
        )
        .unwrap();
    }
    code.push_str("];\n");
    std::fs::write(out_dir.join("web_assets.rs"), code).unwrap();
}
