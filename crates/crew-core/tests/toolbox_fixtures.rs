//! Validate the shared offline fixtures against crew's real schema and resolver.
//! Runtime loading, secrets and native discovery remain downstream acceptance gates.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crew_core::config::{Style, TeamConfig};
use crew_core::toolbox::{
    Content, Fingerprint, Item, ItemKind, ItemRef, Origin, Resolved, SourceKind,
};
use crew_core::RuntimeKind;
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/toolboxes")
}

fn expand(text: &str) -> String {
    text.replace("@ROOT@", "/fixture")
        .replace("@PYTHON@", "/fixture/bin/python")
        .replace("@HTTP_URL@", "http://127.0.0.1:9/mcp")
}

fn team(name: &str) -> TeamConfig {
    let text = fs::read_to_string(fixtures().join("teams").join(name)).unwrap();
    TeamConfig::from_yaml(&expand(&text)).unwrap()
}

fn member(team: &TeamConfig, id: &str) -> Resolved {
    let member = team.agents.iter().find(|agent| agent.id == id).unwrap();
    team.resolve_toolbox(&member.toolbox, &format!("member.{id}"))
        .unwrap()
}

#[test]
fn shared_team_templates_use_the_real_team_schema() {
    for name in [
        "full.yaml",
        "example.yaml",
        "legacy.yaml",
        "legacy-inherit.yaml",
        "resolution.yaml",
    ] {
        let parsed = team(name);
        for agent in &parsed.agents {
            parsed.validate_member_toolbox(agent).unwrap();
        }
        let round_trip = TeamConfig::from_yaml(&parsed.to_yaml().unwrap()).unwrap();
        assert_eq!(parsed, round_trip, "{name}");
    }
    assert!(member(&team("legacy.yaml"), "legacy").items.is_empty());
    assert!(member(&team("legacy-inherit.yaml"), "legacy")
        .sources
        .operator
        .contains(&SourceKind::Mcp));
}

#[test]
fn example_retains_spec_origins_required_items_and_member_exclusions() {
    let team = team("example.yaml");
    let qa = member(&team, "qa");
    let names: Vec<_> = qa.items.keys().map(ToString::to_string).collect();
    assert_eq!(
        names,
        ["mcp.context7", "mcp.playwright", "skills.frontend-qa"]
    );
    let base = &qa.items[&ItemRef::new(ItemKind::Mcp, "context7")];
    assert_eq!(base.origin, Origin::Bundle("base".into()));
    let browser = &qa.items[&ItemRef::new(ItemKind::Mcp, "playwright")];
    assert!(browser.item.required());
    assert_eq!(browser.origin, Origin::Bundle("web".into()));
    let release = member(&team, "release");
    assert!(!release
        .items
        .contains_key(&ItemRef::new(ItemKind::Mcp, "context7")));
    assert_eq!(
        release.items[&ItemRef::new(ItemKind::Skills, "changelog")].origin,
        Origin::Member
    );
    assert!(release
        .items
        .contains_key(&ItemRef::new(ItemKind::Plugins, "lint-kit")));
    assert!(team
        .resolve_toolbox(&team.orchestrator.toolbox, "orchestrator.toolbox")
        .unwrap()
        .items
        .is_empty());
}

#[test]
fn resolution_fixture_covers_readd_whole_replacement_and_policy_reset() {
    let team = team("resolution.yaml");
    let readd = member(&team, "readd");
    assert_eq!(
        readd.items[&ItemRef::new(ItemKind::Mcp, "context7")].origin,
        Origin::Bundle("base".into())
    );
    let replacement = member(&team, "replace");
    let item = &replacement.items[&ItemRef::new(ItemKind::Mcp, "docs")];
    let Item::Mcp(server) = &item.item else {
        panic!("docs must remain an MCP server");
    };
    assert_eq!(item.origin, Origin::Member);
    assert!(server.command.is_empty() && server.env.is_empty() && !server.required);
    assert_eq!(server.url.as_deref(), Some("http://127.0.0.1:9/mcp"));
    let reset = member(&team, "reset");
    assert!(reset.items.is_empty());
    assert!(!reset.strict);
    assert!(reset.sources.operator.is_empty());
    assert_eq!(reset.sources.project.len(), 6);
    assert!(!reset.sources.project.contains(&SourceKind::Mcp));
}

#[test]
fn invalid_fixtures_fail_in_the_actual_validator_with_field_context() {
    let cases: Value =
        serde_json::from_str(&fs::read_to_string(fixtures().join("validation.json")).unwrap())
            .unwrap();
    for case in cases["core"].as_array().unwrap() {
        let yaml = expand(&serde_json::to_string(&case["team"]).unwrap());
        let error = TeamConfig::from_yaml(&yaml).unwrap_err().to_string();
        let field = case["field"].as_str().unwrap();
        assert!(
            error.contains(field),
            "fixture {} expected field {field}: {error}",
            case["name"]
        );
    }
    // These need endpoint/source resolution; never count them as core passes.
    assert_eq!(cases["launch"].as_array().unwrap().len(), 3);
}

#[test]
fn style_fixture_preserves_style_and_home_path_origins() {
    let text = fs::read_to_string(fixtures().join("styles/fixture.yaml")).unwrap();
    let mut style = Style::parse("fixture", &text, false).unwrap();
    style
        .absolutize_toolbox_paths(
            Path::new("/fixture/styles"),
            Some(Path::new("/fixture/home")),
        )
        .unwrap();
    let team = style.team("fixture", "/unrelated/worktree", "offline");
    let resolved = member(&team, "style-member");
    let path = |name| match &resolved.items[&ItemRef::new(ItemKind::Skills, name)].item {
        Item::File(file) => file.path.as_str(),
        _ => panic!("expected a skill"),
    };
    assert_eq!(
        path("style-skill"),
        "/fixture/styles/../items/skills/shared"
    );
    assert_eq!(path("home-skill"), "/fixture/home/fixture-home-skill");
}

fn reverse_object_order(value: &Value) -> String {
    match value {
        Value::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .rev()
                .map(|(key, value)| format!(
                    "{}:{}",
                    serde_json::to_string(key).unwrap(),
                    reverse_object_order(value)
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(reverse_object_order)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).unwrap(),
    }
}

#[test]
fn fixture_key_order_and_same_selection_do_not_change_canonical_bytes() {
    let text = fs::read_to_string(fixtures().join("teams/full.yaml")).unwrap();
    let value: Value = serde_json::from_str(&expand(&text)).unwrap();
    let original = TeamConfig::from_yaml(&serde_json::to_string(&value).unwrap()).unwrap();
    let reordered = TeamConfig::from_yaml(&reverse_object_order(&value)).unwrap();
    let qa = member(&original, "qa");
    let content: BTreeMap<_, _> = qa
        .items
        .iter()
        .filter(|(_, item)| matches!(&item.item, Item::File(_)))
        .map(|(key, _)| (key.clone(), Content::Sha256("a".repeat(64))))
        .collect();
    // Snapshot hashes are supplied by the real launcher later. This checks only
    // the pure schema contract given fixed hashes, not filesystem snapshotting.
    let canonical = qa.canonical(&content).unwrap();
    for resolved in [member(&reordered, "qa"), member(&original, "qa-peer")] {
        let other = resolved.canonical(&content).unwrap();
        assert_eq!(canonical, other);
        assert_eq!(
            Fingerprint::of(&canonical, RuntimeKind::Claude, 1),
            Fingerprint::of(&other, RuntimeKind::Claude, 1)
        );
    }
}
