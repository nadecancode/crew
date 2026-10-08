//! Documentation selection is separate from Git ignore rules and endpoint
//! configuration. Invalid updates must leave the service's last policy intact.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

const MAX_RULES: usize = 32;
pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_DEPTH: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub enabled: Option<bool>,
    pub extra_roots: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub enabled: bool,
    extra_roots: Vec<Relative>,
    exclude: Vec<Relative>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            extra_roots: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

/// A normalized, source-relative name. Constructing one confers no read
/// authority; selection and descriptor-relative open checks remain necessary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Relative(String);

impl Relative {
    pub fn new(value: &str) -> Result<Self, &'static str> {
        if value.is_empty()
            || value.len() > MAX_PATH_BYTES
            || value.contains('\\')
            || value.chars().any(char::is_control)
            || Path::new(value).is_absolute()
        {
            return Err("expected a bounded relative documentation path");
        }
        let mut parts = Vec::new();
        for part in Path::new(value).components() {
            match part {
                Component::Normal(name) => parts.push(name.to_str().ok_or("path is not UTF-8")?),
                Component::CurDir => {}
                _ => return Err("documentation paths cannot contain parent components"),
            }
        }
        if parts.is_empty() {
            return Err("documentation path is empty");
        }
        Ok(Self(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
    pub fn depth(&self) -> usize {
        self.0.split('/').count()
    }
    pub fn join(&self, name: &str) -> Result<Self, &'static str> {
        Self::new(&format!("{}/{name}", self.0))
    }
    pub fn markdown(&self) -> bool {
        self.as_path()
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("md") || s.eq_ignore_ascii_case("markdown"))
    }
    pub fn forbidden(&self) -> bool {
        self.0.split('/').any(|part| {
            part.starts_with('.')
                || [
                    "node_modules",
                    "target",
                    "dist",
                    "build",
                    "out",
                    "coverage",
                    "vendor",
                    "secrets",
                    "credentials",
                ]
                .iter()
                .any(|name| part.eq_ignore_ascii_case(name))
        })
    }
}

impl Policy {
    pub fn layered(layers: impl IntoIterator<Item = Layer>) -> Result<Self, &'static str> {
        let mut policy = Self::default();
        for layer in layers {
            if let Some(enabled) = layer.enabled {
                policy.enabled = enabled;
            }
            if let Some(paths) = layer.extra_roots {
                policy.extra_roots = rules(paths)?;
            }
            if let Some(paths) = layer.exclude {
                policy.exclude = rules(paths)?;
            }
        }
        Ok(policy)
    }

    pub fn allows(&self, path: &Relative) -> bool {
        self.enabled
            && !path.forbidden()
            && path.depth() <= MAX_DEPTH + 1
            && !self
                .exclude
                .iter()
                .any(|p| path.as_path().starts_with(p.as_path()))
    }

    pub fn selects(&self, path: &Relative) -> bool {
        self.allows(path)
            && path.markdown()
            && (path.depth() <= 2
                || self
                    .recursive_roots()
                    .iter()
                    .any(|root| path.as_path().starts_with(root)))
    }

    pub fn descends(&self, path: &Relative) -> bool {
        self.allows(path)
            && path.depth() <= MAX_DEPTH
            && (path.depth() == 1
                || self.recursive_roots().iter().any(|root| {
                    path.as_path().starts_with(root) || root.starts_with(path.as_path())
                }))
    }

    fn recursive_roots(&self) -> Vec<PathBuf> {
        ["notes", "review", "docs"]
            .into_iter()
            .map(PathBuf::from)
            .chain(self.extra_roots.iter().map(|p| p.as_path().to_owned()))
            .collect()
    }

    /// Parse only [docs]; unrelated configuration values are never exposed.
    pub fn from_toml_layers<'a>(
        layers: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, &'static str> {
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            docs: Option<Layer>,
        }
        let mut parsed = Vec::new();
        for text in layers {
            if text.len() > 1024 * 1024 {
                return Err("documentation configuration exceeds limit");
            }
            let file: File =
                toml::from_str(text).map_err(|_| "invalid documentation configuration")?;
            if let Some(docs) = file.docs {
                parsed.push(docs);
            }
        }
        Self::layered(parsed)
    }
}

fn rules(values: Vec<String>) -> Result<Vec<Relative>, &'static str> {
    if values.len() > MAX_RULES {
        return Err("too many documentation rules");
    }
    values
        .iter()
        .map(|value| {
            let path = Relative::new(value)?;
            if path.forbidden() {
                return Err("documentation rule names a hard exclusion");
            }
            Ok(path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn k01_k02_selection_and_layer_replacement_are_explicit() {
        let p = Policy::from_toml_layers([
            "[docs]\nextra_roots=['design']\nexclude=['notes/private']",
            "[docs]\nextra_roots=['crates/crew/manual.md']",
        ])
        .unwrap();
        let yes = [
            "README.md",
            "examples/readme.MD",
            "notes/deep/x.md",
            "review/a/b.markdown",
            "crates/crew/manual.md",
        ];
        for name in yes {
            assert!(p.selects(&Relative::new(name).unwrap()), "{name}");
        }
        for name in [
            "design/deep/x.md",
            "src/deep/README.md",
            "notes/private/secret.md",
            "notes/private",
            "notes/a.txt",
        ] {
            assert!(!p.selects(&Relative::new(name).unwrap()), "{name}");
        }
        assert!(p.descends(&Relative::new("crates/crew").unwrap()));
        assert!(!p.selects(&Relative::new("crates/crew/README.md").unwrap()));
        assert!(p.selects(&Relative::new("notes/private-extra.md").unwrap()));
    }

    #[test]
    fn k03_hard_exclusions_cannot_be_extended_or_encoded_as_parent_paths() {
        for name in ["", "/etc", "notes/../secrets", "a\\b", "a\0b", "."] {
            assert!(Relative::new(name).is_err(), "{name:?}");
        }
        for name in [
            ".crew/worktrees",
            "docs/.git/x.md",
            "docs/Target/x.md",
            "SECRETS/x.md",
            ".hidden.md",
        ] {
            let p = Relative::new(name).unwrap();
            assert!(!Policy::default().allows(&p));
            assert!(Policy::layered([Layer {
                extra_roots: Some(vec![name.into()]),
                ..Default::default()
            }])
            .is_err());
        }
        // The absolute member-root ancestors are not content-path components.
        assert!(Policy::default().selects(&Relative::new("notes/x.md").unwrap()));
        let bad = "[claude]\napi_key='do-not-quote'\n[docs]\nenabled='do-not-quote'";
        assert!(!Policy::from_toml_layers([bad])
            .unwrap_err()
            .contains("do-not-quote"));
    }
}
