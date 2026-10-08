//! Toolbox credentials have a separate lookup from model endpoint variables.
//! These values stay in daemon memory and are delivered only to the MCP proxy.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use crew_core::toolbox::{McpServer, Reference, PROTECTED_ENV};
use crew_core::RuntimeKind;
use serde::Deserialize;

use super::{expand_path, is_env_name, read_token_file, Layers, Secret, Source};

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum SecretSource {
    Env { env: String },
    File { file: String },
}

pub(super) fn validate_sources(sources: &BTreeMap<String, SecretSource>) -> Result<()> {
    for (name, source) in sources {
        if !is_env_name(name) {
            bail!("toolbox_env: '{name}' is not an environment variable name");
        }
        match source {
            SecretSource::Env { env } if !is_env_name(env) => {
                bail!("toolbox_env.{name}.env is not an environment variable name");
            }
            SecretSource::File { file } if file.trim().is_empty() || file.contains('\0') => {
                bail!("toolbox_env.{name}.file must name a nonempty file path");
            }
            _ => {}
        }
    }
    Ok(())
}

/// Only this transient result contains credential values. It intentionally
/// has no Serialize implementation; Secret's Debug implementation is redacted.
#[derive(Clone, Debug, Default)]
pub struct ToolboxReferences {
    pub env: BTreeMap<String, Secret>,
    pub headers: BTreeMap<String, Secret>,
    /// Both raw and formatted secret values, for best-effort response redaction.
    pub redactions: Vec<(String, Secret)>,
    /// Reference names and their daemon-environment aliases, never destinations.
    pub scrub: BTreeSet<String>,
    pub missing: BTreeSet<String>,
}

impl Layers {
    /// The model and endpoint variables a toolbox must never borrow or scrub.
    pub fn toolbox_protected_names(
        &self,
        runtime: RuntimeKind,
        agent: &str,
        parent: Option<&str>,
    ) -> Result<BTreeSet<String>> {
        self.check()?;
        let plan = self.plan(runtime, agent, parent);
        let mut names: BTreeSet<String> = PROTECTED_ENV.iter().map(|name| (*name).into()).collect();
        names.extend(plan.env.keys().cloned());
        names.extend(plan.env_files.keys().cloned());
        if let Some((_, Source::Env(name), _)) = plan.credential {
            names.insert(name);
        }
        Ok(names)
    }

    fn toolbox_sources(&self, agent: &str, parent: Option<&str>) -> BTreeMap<String, Source> {
        let mut sources = BTreeMap::new();
        let mut apply = |table: &BTreeMap<String, SecretSource>, directory: &Path| {
            for (name, source) in table {
                let source = match source {
                    SecretSource::Env { env } => Source::Env(env.clone()),
                    SecretSource::File { file } => Source::File(expand_path(file, directory)),
                };
                sources.insert(name.clone(), source);
            }
        };
        for (layer, config) in self.loaded() {
            apply(
                &config.toolbox_env,
                layer.path.parent().unwrap_or(Path::new("/")),
            );
        }
        for id in parent.into_iter().chain([agent]) {
            for (layer, config) in self.loaded() {
                if let Some(section) = config.agents.get(id) {
                    apply(
                        &section.toolbox_env,
                        layer.path.parent().unwrap_or(Path::new("/")),
                    );
                }
            }
        }
        sources
    }

    pub fn toolbox_references(
        &self,
        runtime: RuntimeKind,
        agent: &str,
        parent: Option<&str>,
        server: &McpServer,
    ) -> Result<ToolboxReferences> {
        self.toolbox_references_with(runtime, agent, parent, server, |name| {
            std::env::var(name).ok()
        })
    }

    fn toolbox_references_with(
        &self,
        runtime: RuntimeKind,
        agent: &str,
        parent: Option<&str>,
        server: &McpServer,
        read_env: impl Fn(&str) -> Option<String>,
    ) -> Result<ToolboxReferences> {
        let protected = self.toolbox_protected_names(runtime, agent, parent)?;
        let sources = self.toolbox_sources(agent, parent);
        let mut result = ToolboxReferences::default();
        // Check every name before reading any value. In particular an alias
        // cannot borrow a model key under a different reference name.
        for reference in server.env.values().chain(server.headers.values()) {
            if let Reference::Env { name, .. } = reference {
                let source = sources
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| Source::Env(name.clone()));
                let mut names = vec![name.as_str()];
                if let Source::Env(alias) = &source {
                    names.push(alias);
                }
                for name in names {
                    if protected.contains(name) {
                        bail!("toolbox reference/source ${name} is reserved by the member's runtime or endpoint");
                    }
                    // Loader-affecting variables cannot be handed to the proxy either.
                    crate::sandbox::spec::check_name(name)
                        .map_err(|_| anyhow!("toolbox reference/source ${name} is not allowed"))?;
                    result.scrub.insert(name.to_string());
                }
            }
        }
        for (headers, references) in [(false, &server.env), (true, &server.headers)] {
            for (destination, reference) in references {
                let value = match reference {
                    Reference::Value(value) => Secret::new(value),
                    Reference::Env { name, format } => {
                        let source = sources
                            .get(name)
                            .cloned()
                            .unwrap_or_else(|| Source::Env(name.clone()));
                        let value = match source {
                            Source::Env(alias) => read_env(&alias)
                                .filter(|value| !value.trim().is_empty())
                                .map(Secret::new),
                            Source::File(path) => match read_token_file(&path) {
                                Ok(value) => Some(value),
                                Err(error)
                                    if error.downcast_ref::<std::io::Error>().is_some_and(
                                        |e| e.kind() == std::io::ErrorKind::NotFound,
                                    ) =>
                                {
                                    None
                                }
                                Err(error) => {
                                    return Err(error)
                                        .with_context(|| format!("toolbox reference ${name}"));
                                }
                            },
                            Source::Literal(_) => {
                                unreachable!("toolbox sources never hold literals")
                            }
                        };
                        let Some(value) = value else {
                            result.missing.insert(name.clone());
                            continue;
                        };
                        if value.expose().contains('\0') {
                            bail!("toolbox reference ${name} contains NUL");
                        }
                        result.redactions.push((name.clone(), value.clone()));
                        match format {
                            Some(format) => {
                                let formatted = Secret::new(format.replace("{}", value.expose()));
                                result.redactions.push((name.clone(), formatted.clone()));
                                formatted
                            }
                            None => value,
                        }
                    }
                };
                if headers {
                    result.headers.insert(destination.clone(), value);
                } else {
                    result.env.insert(destination.clone(), value);
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn server() -> McpServer {
        McpServer {
            command: vec!["fixture".into()],
            env: BTreeMap::from([(
                "DESTINATION".into(),
                Reference::Env {
                    name: "SHOP_TOKEN".into(),
                    format: None,
                },
            )]),
            ..McpServer::default()
        }
    }

    #[test]
    fn member_and_parent_sources_override_globals_without_entering_endpoint_environment() {
        let layers = super::super::tests::layers(
            "[toolbox_env]\nSHOP_TOKEN={env='GLOBAL_TOKEN'}\n[agents.parent.toolbox_env]\nSHOP_TOKEN={env='PARENT_TOKEN'}",
            "[toolbox_env]\nSHOP_TOKEN={env='USER_TOKEN'}",
            "[agents.child.toolbox_env]\nSHOP_TOKEN={env='CHILD_TOKEN'}",
        );
        let get = |name: &str| Some(format!("value-for-{name}"));
        let result = layers
            .toolbox_references_with(RuntimeKind::Claude, "child", Some("parent"), &server(), get)
            .unwrap();
        assert_eq!(result.env["DESTINATION"].expose(), "value-for-CHILD_TOKEN");
        assert_eq!(
            result.scrub,
            BTreeSet::from(["SHOP_TOKEN".into(), "CHILD_TOKEN".into()])
        );
        let sibling = layers
            .toolbox_references_with(
                RuntimeKind::Claude,
                "sibling",
                Some("parent"),
                &server(),
                get,
            )
            .unwrap();
        assert_eq!(
            sibling.env["DESTINATION"].expose(),
            "value-for-PARENT_TOKEN"
        );
        let endpoint = layers
            .resolve(RuntimeKind::Claude, "child", Some("parent"))
            .unwrap();
        assert!(endpoint.env.is_empty());
        assert!(endpoint.secret_env.is_empty());
        assert!(!format!("{result:?}").contains("value-for"));
    }

    #[test]
    fn aliases_cannot_borrow_model_credentials_or_endpoint_variables() {
        for source in ["OPENAI_API_KEY", "MODEL_LOGIN", "ENDPOINT_SENTINEL"] {
            let config = format!(
                "[codex]\napi_key_env='MODEL_LOGIN'\n[codex.env_files]\nENDPOINT_SENTINEL='/not/read'\n[toolbox_env]\nSHOP_TOKEN={{env='{source}'}}"
            );
            let layers = super::super::tests::layers(&config, "", "");
            let error = layers
                .toolbox_references_with(RuntimeKind::Codex, "member", None, &server(), |_| {
                    panic!("protected names must be rejected before any value is read")
                })
                .unwrap_err();
            assert!(error.to_string().contains(source));
        }
    }

    #[test]
    fn private_files_and_missing_aliases_keep_values_out_of_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let token = temp.path().join("token");
        std::fs::write(&token, "private-toolbox-value\n").unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = format!(
            "[toolbox_env]\nSHOP_TOKEN={{file={:?}}}",
            token.display().to_string()
        );
        let layers = super::super::tests::layers(&config, "", "");
        let mut server = server();
        server.headers.insert(
            "Authorization".into(),
            Reference::Env {
                name: "MISSING".into(),
                format: Some("Bearer {}".into()),
            },
        );
        let result = layers
            .toolbox_references_with(RuntimeKind::Claude, "a", None, &server, |_| None)
            .unwrap();
        assert_eq!(result.env["DESTINATION"].expose(), "private-toolbox-value");
        assert_eq!(result.missing, BTreeSet::from(["MISSING".into()]));
        assert_eq!(
            result.scrub,
            BTreeSet::from(["MISSING".into(), "SHOP_TOKEN".into()])
        );
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = layers
            .toolbox_references_with(RuntimeKind::Claude, "a", None, &server, |_| None)
            .unwrap_err();
        assert!(!format!("{error:#}").contains("private-toolbox-value"));
        assert!(format!("{error:#}").contains("chmod 600"));
    }
}
