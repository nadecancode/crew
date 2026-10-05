//! Typed final-outcome transport through ClientApi updates; engine behavior is
//! exercised separately by the actual planner/reducer/driver consumers.

use crew_client::*;
use serde_json::json;

#[test]
fn ordinary_state_update_has_no_outcome_and_completion_carries_exactly_one_typed_notice() {
    let scope = ClientScope {
        endpoint_id: "local:profile-a".into(),
        workspace_id: "946d4617aa702870".into(),
        generation: Some(1),
        daemon_pid: Some(1),
        daemon_started_at_ms: Some(1234),
        attempt: 1,
    };
    let ordinary = ClientUpdate {
        revision: 1,
        changes: ChangeSet {
            revision: 1,
            agents: vec!["claude-1".into()],
            ..ChangeSet::default()
        },
        effects: vec![],
        outcomes: vec![],
    };
    assert_eq!(
        serde_json::to_value(&ordinary).unwrap()["outcomes"],
        json!([])
    );
    let restored: ClientUpdate =
        serde_json::from_value(serde_json::to_value(&ordinary).unwrap()).unwrap();
    assert!(restored.outcomes.is_empty());
    let notice = OperationNotice {
        scope,
        operation_id: 203,
        attachment_id: Some(101),
        kind: OperationOutcomeKind::SentUnacknowledged,
        reason: None,
    };
    let completed = ClientUpdate {
        revision: 2,
        changes: ChangeSet {
            revision: 2,
            pending: true,
            ..ChangeSet::default()
        },
        effects: vec![],
        outcomes: vec![notice.clone()],
    };
    let restored: ClientUpdate =
        serde_json::from_value(serde_json::to_value(&completed).unwrap()).unwrap();
    assert_eq!(restored.outcomes, [notice]);
    assert!(restored.effects.is_empty());
    assert_ne!(
        restored.outcomes[0].operation_id,
        restored.outcomes[0].attachment_id.unwrap()
    );
    let planned = PlanUpdate {
        plan: ActionPlan::Dispatch { operation_id: 204 },
        update: restored.clone(),
    };
    let restored_plan: PlanUpdate =
        serde_json::from_value(serde_json::to_value(planned).unwrap()).unwrap();
    assert_eq!(restored_plan.update.outcomes, restored.outcomes);
}
