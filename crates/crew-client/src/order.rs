//! Stable agent ordering used by the terminal dashboard.

use std::cmp::Ordering;
use std::collections::HashMap;

use crew_protocol::{Agent, ORCHESTRATOR_ID};

/// Order ids so that `claude-2` sorts before `claude-10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn split(s: &str) -> (&str, Option<u64>) {
        let digits = s.bytes().rev().take_while(u8::is_ascii_digit).count();
        let (head, tail) = s.split_at(s.len() - digits);
        (head, tail.parse().ok())
    }
    let (ah, an) = split(a);
    let (bh, bn) = split(b);
    ah.cmp(bh).then(an.cmp(&bn)).then(a.cmp(b))
}

/// Both the reserved id and an explicitly designated orchestrator are recognized.
pub fn is_orchestrator(agent: &Agent) -> bool {
    agent.orchestrator || agent.id == ORCHESTRATOR_ID
}

/// The known top-level worker a subagent hangs off, if any.
///
/// Orphans, children of the orchestrator and nested subagents appear as ordinary
/// members. The returned id borrows the parent from `agents`, not the child.
pub fn satellite_parent_id<'a>(agents: &'a [Agent], agent: &Agent) -> Option<&'a str> {
    if is_orchestrator(agent) {
        return None;
    }
    let parent_id = agent.parent.as_deref()?;
    let parent = agents.iter().find(|a| a.id == parent_id)?;
    (parent.id != agent.id && !is_orchestrator(parent) && parent.parent.is_none())
        .then_some(parent.id.as_str())
}

/// Workers in natural id order, each followed by its subagents, then orchestrators.
/// Equal keys retain the input order.
pub fn sort_agents(agents: &mut [Agent]) {
    let roots: HashMap<String, String> = agents
        .iter()
        .filter_map(|a| satellite_parent_id(agents, a).map(|p| (a.id.clone(), p.to_string())))
        .collect();
    agents.sort_by(|a, b| {
        let root_a = roots.get(&a.id).map_or(a.id.as_str(), String::as_str);
        let root_b = roots.get(&b.id).map_or(b.id.as_str(), String::as_str);
        is_orchestrator(a)
            .cmp(&is_orchestrator(b))
            .then_with(|| natural_cmp(root_a, root_b))
            .then_with(|| roots.contains_key(&a.id).cmp(&roots.contains_key(&b.id)))
            .then_with(|| natural_cmp(&a.id, &b.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: &str, parent: Option<&str>) -> Agent {
        serde_json::from_value(json!({
            "id": id, "name": id, "role": "", "status": "idle", "runtime": "codex",
            "workdir": "", "backend": "pty", "last_activity_at": 0, "parent": parent,
        }))
        .unwrap()
    }

    #[test]
    fn natural_order_preserves_suffix_and_overflow_rules() {
        for (a, b, expected) in [
            ("claude-2", "claude-10", Ordering::Less),
            ("architect", "db", Ordering::Less),
            ("a", "a0", Ordering::Less),
            ("a01", "a1", Ordering::Less),
            ("a18446744073709551616", "a1", Ordering::Less),
            (
                "a18446744073709551615",
                "a18446744073709551616",
                Ordering::Greater,
            ),
            ("é2", "é10", Ordering::Less),
            ("same", "same", Ordering::Equal),
        ] {
            assert_eq!(natural_cmp(a, b), expected, "{a} versus {b}");
            assert_eq!(natural_cmp(b, a), expected.reverse(), "{b} versus {a}");
        }
    }

    #[test]
    fn satellites_require_a_known_top_level_worker() {
        let mut designated = agent("lead", None);
        designated.orchestrator = true;
        let agents = vec![
            agent("worker", None),
            agent("child", Some("worker")),
            agent("nested", Some("child")),
            agent("orphan", Some("missing")),
            agent("self", Some("self")),
            agent("orchestrator", None),
            agent("orch-child", Some("orchestrator")),
            designated,
            agent("lead-child", Some("lead")),
        ];
        for (id, expected) in [
            ("worker", None),
            ("child", Some("worker")),
            ("nested", None),
            ("orphan", None),
            ("self", None),
            ("orchestrator", None),
            ("orch-child", None),
            ("lead", None),
            ("lead-child", None),
        ] {
            let a = agents.iter().find(|a| a.id == id).unwrap();
            assert_eq!(satellite_parent_id(&agents, a), expected, "{id}");
        }
        let mut designated_child = agent("designated-child", Some("worker"));
        designated_child.orchestrator = true;
        assert!(satellite_parent_id(&agents, &designated_child).is_none());
    }

    #[test]
    fn stable_sort_groups_children_and_keeps_orphans_on_the_ring() {
        let mut agents = vec![
            agent("orchestrator", None),
            agent("worker-10", None),
            agent("worker-2-s10", Some("worker-2")),
            agent("worker-2-s2", Some("worker-2")),
            agent("worker-2", None),
            agent("orphan", Some("gone")),
        ];
        sort_agents(&mut agents);
        let ids: Vec<_> = agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "orphan",
                "worker-2",
                "worker-2-s2",
                "worker-2-s10",
                "worker-10",
                "orchestrator"
            ]
        );
        agents.retain(|a| a.id != "worker-2");
        sort_agents(&mut agents);
        assert!(agents
            .iter()
            .all(|a| satellite_parent_id(&agents, a).is_none()));
        assert_eq!(agents.last().unwrap().id, "orchestrator");
    }

    #[test]
    fn equal_sort_keys_keep_input_order_and_flagged_lead_sorts_last() {
        let mut first = agent("worker", None);
        first.name = "first".into();
        let mut second = first.clone();
        second.name = "second".into();
        let mut lead = agent("a", None);
        lead.orchestrator = true;
        let mut agents = vec![lead, first, second];
        sort_agents(&mut agents);
        assert_eq!(agents[0].name, "first");
        assert_eq!(agents[1].name, "second");
        assert_eq!(agents[2].id, "a");
    }
}
