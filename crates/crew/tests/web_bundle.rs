//! The committed dashboard bundle (`crates/crew/web-dist/`) matches its
//! manifest and the sources it was built from, without node
//! (notes/web-design.md §build, §tests). Without a bundle, `/` is the
//! built-in page that says so.

mod common;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use crew_core::rpc::WebInfo;
use crew_core::util::fnv1a64;
use serde_json::Value;

/// P4: the whole of `web-dist/`.
const MAX_BUNDLE_BYTES: u64 = 600_000;

fn dist() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("web-dist")
}

fn web_sources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web")
}

fn hash(bytes: &[u8]) -> String {
    format!("fnv1a64:{:016x}", fnv1a64(bytes))
}

/// Every file below `dir`, relative to `base` with `/`, sorted bytewise.
fn walk(dir: &Path, base: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path, base));
        } else {
            let rel = path.strip_prefix(base).unwrap();
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push(parts.join("/"));
        }
    }
    out.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    out
}

/// The fingerprint of the build's inputs, as `web/scripts/stamp.ts` writes
/// it: each input's path, a zero byte, its length in decimal, a zero byte
/// and its bytes, through one FNV-1a 64.
fn source_fingerprint(web: &Path) -> String {
    let mut inputs: Vec<String> = [
        "index.html",
        "package.json",
        "bun.lock",
        "tsconfig.json",
        "vite.config.ts",
    ]
    .map(String::from)
    .to_vec();
    inputs.extend(walk(&web.join("src"), web));
    let mut stream = Vec::new();
    for path in &inputs {
        let bytes = std::fs::read(web.join(path))
            .unwrap_or_else(|e| panic!("web/{path}, an input of the build: {e}"));
        stream.extend_from_slice(path.as_bytes());
        stream.push(0);
        stream.extend_from_slice(bytes.len().to_string().as_bytes());
        stream.push(0);
        stream.extend_from_slice(&bytes);
    }
    hash(&stream)
}

/// What `index.html` refers to by `src` and `href`, outside the page.
fn references(index: &str) -> Vec<String> {
    let mut out = Vec::new();
    for attribute in ["src=\"", "href=\""] {
        let mut rest = index;
        while let Some(at) = rest.find(attribute) {
            rest = &rest[at + attribute.len()..];
            let Some(end) = rest.find('"') else { break };
            let value = &rest[..end];
            if !value.is_empty()
                && !value.starts_with('#')
                && !value.starts_with("data:")
                && !value.contains("://")
            {
                out.push(
                    value
                        .trim_start_matches("./")
                        .trim_start_matches('/')
                        .to_string(),
                );
            }
        }
    }
    out
}

/// `/` of a daemon serving this build.
fn served_index() -> Vec<u8> {
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();
    s.ok(&["start", "-C", &ws]);
    let info: WebInfo = serde_json::from_str(&s.ok(&["web", "--json", "-C", &ws])).unwrap();
    let host = info
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", info.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a response head");
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    response[split + 4..].to_vec()
}

#[test]
fn the_bundle_is_what_its_manifest_and_sources_say() {
    let dist = dist();
    let manifest = dist.join("manifest.json");
    if !manifest.exists() {
        assert!(
            !dist.join("index.html").exists(),
            "web-dist/ has a bundle but no manifest.json: run `cd web && bun run build`"
        );
        let index = String::from_utf8(served_index()).unwrap();
        assert!(
            index.contains("The web dashboard is not part of this build of crew."),
            "{index}"
        );
        return;
    }
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap())
        .expect("manifest.json is JSON");
    assert_eq!(manifest["version"], 1);

    // Every listed file, with its size and hash; nothing unlisted.
    let listed: Vec<&Value> = manifest["files"].as_array().unwrap().iter().collect();
    let mut paths = Vec::new();
    let mut total = std::fs::metadata(dist.join("manifest.json")).unwrap().len();
    for file in &listed {
        let path = file["path"].as_str().unwrap();
        let bytes = std::fs::read(dist.join(path))
            .unwrap_or_else(|e| panic!("web-dist/{path} is listed: {e}"));
        assert_eq!(file["bytes"], bytes.len(), "{path}: size");
        assert_eq!(file["hash"], hash(&bytes), "{path}: hash");
        total += bytes.len() as u64;
        paths.push(path.to_string());
    }
    let present: Vec<String> = walk(&dist, &dist)
        .into_iter()
        .filter(|p| p != "manifest.json")
        .collect();
    assert_eq!(present, paths, "the files of web-dist/ and the manifest's");
    assert!(
        total <= MAX_BUNDLE_BYTES,
        "web-dist/ is {total} bytes, over the {MAX_BUNDLE_BYTES} of P4"
    );

    // The page names only files of the bundle.
    let index = std::fs::read_to_string(dist.join("index.html")).unwrap();
    for reference in references(&index) {
        assert!(
            paths.contains(&reference),
            "index.html refers to {reference}, which is not in the bundle"
        );
    }

    // Built from these sources.
    let web = web_sources();
    assert!(web.join("package.json").exists(), "web/ is missing");
    assert_eq!(
        manifest["source"],
        source_fingerprint(&web),
        "web-dist/ was built from other sources than web/: run `cd web && bun run build` and commit \
the bundle"
    );

    // And it is what the daemon serves.
    assert!(
        served_index() == index.as_bytes(),
        "the daemon serves another index.html than web-dist/ has: build crew again"
    );
}

#[test]
fn references_are_read_from_src_and_href() {
    let page = r##"<link rel="icon" href="data:,"><script type="module" src="/assets/app-1.js"></script>
<link rel="stylesheet" href="./assets/app-2.css"><a href="#main">skip</a><a href="https://x.example/">x</a>"##;
    assert_eq!(
        references(page),
        vec![
            "assets/app-1.js".to_string(),
            "assets/app-2.css".to_string()
        ]
    );
}
