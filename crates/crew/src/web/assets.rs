//! The dashboard's files, embedded by `build.rs` from `web-dist/`. No path
//! a browser sends ever reaches the file system: it is looked up in this
//! table, exactly.

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

/// One embedded file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Asset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

/// The file at `path` (without its leading `/`), if the bundle has one.
pub(crate) fn find(path: &str) -> Option<Asset> {
    ASSETS
        .binary_search_by(|(p, _, _)| p.as_bytes().cmp(path.as_bytes()))
        .ok()
        .map(|i| {
            let (path, content_type, bytes) = ASSETS[i];
            Asset {
                path,
                content_type,
                bytes,
            }
        })
}

/// Every embedded file.
#[cfg(test)]
pub(crate) fn all() -> impl Iterator<Item = Asset> {
    ASSETS.iter().map(|&(path, content_type, bytes)| Asset {
        path,
        content_type,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_path_answers_with_its_type_and_nothing_else_does() {
        let index = find("index.html").expect("an index.html is always embedded");
        assert_eq!(index.content_type, "text/html; charset=utf-8");
        for asset in all() {
            assert_eq!(find(asset.path), Some(asset));
            assert!(!asset.path.starts_with('/'), "{}", asset.path);
        }
        let paths: Vec<_> = all().map(|a| a.path).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "the table must be sorted for the search");
        for path in [
            "",
            "/index.html",
            "nope",
            "../Cargo.toml",
            "assets/../index.html",
            "index.html%00",
            "%2e%2e/Cargo.toml",
            "index.html/",
            "INDEX.HTML",
        ] {
            assert_eq!(find(path), None, "{path}");
        }
    }
}
