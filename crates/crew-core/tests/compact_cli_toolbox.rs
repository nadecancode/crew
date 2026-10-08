use std::collections::BTreeMap;

use crew_core::config::{Style, TeamConfig};
use crew_core::toolbox::{
    self, BuiltinSkill, Catalog, Item, ItemKind, ItemRef, Toolbox, COMPACT_CLI,
};
use serde_json::json;

fn resolve(
    bundles: &BTreeMap<String, Toolbox>,
    defaults: &Toolbox,
    member: &Toolbox,
) -> toolbox::Resolved {
    let catalog = Catalog {
        bundles,
        defaults,
        inherit_mcp: false,
    };
    toolbox::validate_catalog(catalog).unwrap();
    toolbox::check_includes(member, bundles, "member").unwrap();
    toolbox::resolve(catalog, member, "member").unwrap()
}

#[test]
fn builtin_is_opt_in_and_keeps_normal_reset_exclusion_and_override_rules() {
    let bundles = BTreeMap::new();
    let empty = Toolbox::default();
    let selected: Toolbox = serde_json::from_value(json!({"include":[COMPACT_CLI]})).unwrap();
    let key = ItemRef::new(ItemKind::Skills, COMPACT_CLI);
    assert!(resolve(&bundles, &empty, &empty).items.is_empty());
    let active = resolve(&bundles, &empty, &selected);
    assert_eq!(active.items.len(), 1);
    assert_eq!(
        active.items[&key].item,
        Item::Builtin(BuiltinSkill::CompactCli)
    );
    assert_eq!(active.items[&key].origin.to_string(), "bundle:compact-cli");
    for member in [
        json!({"reset":true}),
        json!({"exclude":["skills.compact-cli"]}),
    ] {
        assert!(resolve(
            &bundles,
            &selected,
            &serde_json::from_value(member).unwrap()
        )
        .items
        .is_empty());
    }
    let member: Toolbox =
        serde_json::from_value(json!({"skills":{"compact-cli":{"path":"/selected/skill"}}}))
            .unwrap();
    assert!(matches!(
        resolve(&bundles, &selected, &member).items[&key].item,
        Item::File(_)
    ));
    // A same-name user bundle is a whole replacement, even an empty one.
    let bundles = BTreeMap::from([(COMPACT_CLI.into(), empty.clone())]);
    assert!(resolve(&bundles, &empty, &selected).items.is_empty());
}

#[test]
fn builtin_survives_team_style_roundtrips_and_nested_inclusion_without_path_magic() {
    let team = TeamConfig::from_yaml("project: p\nworkspace: {path: /fixture}\ntoolboxes:\n  tests: {include: [compact-cli]}\nagents:\n  - id: a\n    role: tests\n    toolbox: {include: [tests]}\n").unwrap();
    let before = team.resolve_toolbox(&team.agents[0].toolbox, "a").unwrap();
    let after = TeamConfig::from_yaml(&team.to_yaml().unwrap()).unwrap();
    assert_eq!(
        before,
        after
            .resolve_toolbox(&after.agents[0].toolbox, "a")
            .unwrap()
    );
    let mut style = Style::parse(
        "tests",
        "defaults:\n  toolbox: {include: [compact-cli]}\n",
        false,
    )
    .unwrap();
    style
        .absolutize_toolbox_paths(std::path::Path::new("/unrelated"), None)
        .unwrap();
    assert_eq!(
        before.items,
        style
            .team("p", "/fixture", "")
            .resolve_toolbox(&Toolbox::default(), "member")
            .unwrap()
            .items
    );
}
