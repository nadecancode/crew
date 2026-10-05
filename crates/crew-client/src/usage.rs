//! Optional reported counters and account-window facts, without formatting,
//! pricing, geometry, or an inferred report for a silent agent.

use std::collections::{BTreeMap, VecDeque};

use crew_protocol::{rpc::SessionInfo, Agent, RuntimeKind, Usage};

use crate::api::{AgentUsage, ContextPoint, QuotaRow, RuntimeQuotaGroup, UsagePresentation};

const STALE_MS: i128 = 20 * 60_000;
const QUOTA_RUNTIMES: [RuntimeKind; 3] = [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Pi];

fn add_counter(total: &mut u64, value: u64, overflowed: &mut bool) {
    match total.checked_add(value) {
        Some(value) => *total = value,
        None => {
            *total = u64::MAX;
            *overflowed = true;
        }
    }
}

pub fn usage_data(
    agents: &[Agent],
    session: Option<&SessionInfo>,
    samples: &BTreeMap<String, VecDeque<ContextPoint>>,
    server_now_ms: i64,
) -> UsagePresentation {
    let mut total = Usage::default();
    let mut reporters = 0_u32;
    let mut overflowed = false;
    for usage in agents.iter().filter_map(|agent| agent.usage.as_ref()) {
        if let Some(count) = reporters.checked_add(1) {
            reporters = count;
        } else {
            overflowed = true;
        }
        add_counter(&mut total.input_tokens, usage.input_tokens, &mut overflowed);
        add_counter(
            &mut total.output_tokens,
            usage.output_tokens,
            &mut overflowed,
        );
        add_counter(
            &mut total.cache_read_tokens,
            usage.cache_read_tokens,
            &mut overflowed,
        );
        add_counter(
            &mut total.cache_creation_tokens,
            usage.cache_creation_tokens,
            &mut overflowed,
        );
        add_counter(&mut total.turns, usage.turns, &mut overflowed);
    }
    // Every category can fit independently while their reported-token sum does
    // not. An overflowing aggregate is unavailable; individual reports remain.
    let reported_tokens = u128::from(total.input_tokens)
        + u128::from(total.output_tokens)
        + u128::from(total.cache_read_tokens)
        + u128::from(total.cache_creation_tokens);
    overflowed |= reported_tokens > u128::from(u64::MAX);
    let eligible = u32::try_from(agents.len()).unwrap_or_else(|_| {
        overflowed = true;
        u32::MAX
    });
    let rows = agents
        .iter()
        .map(|agent| AgentUsage {
            agent_id: agent.id.clone(),
            usage: agent.usage.clone(),
            context: agent.context.clone(),
            samples: samples
                .get(&agent.id)
                .map(|points| points.iter().copied().collect())
                .unwrap_or_default(),
            compactions: agent.compactions,
        })
        .collect();
    let groups = QUOTA_RUNTIMES
        .into_iter()
        .filter_map(|runtime| {
            let quotas: Vec<_> = session
                .into_iter()
                .flat_map(|session| &session.quotas)
                .filter(|quota| quota.runtime == runtime)
                .map(|quota| QuotaRow {
                    quota: quota.clone(),
                    stale: i128::from(server_now_ms) - i128::from(quota.updated_at) > STALE_MS,
                })
                .collect();
            let paused_agent_ids: Vec<_> = agents
                .iter()
                .filter(|agent| agent.runtime == runtime && agent.limit.is_some())
                .map(|agent| agent.id.clone())
                .collect();
            (!quotas.is_empty() || !paused_agent_ids.is_empty()).then_some(RuntimeQuotaGroup {
                runtime,
                quotas,
                paused_agent_ids,
            })
        })
        .collect();
    UsagePresentation {
        total: (reporters != 0 && !overflowed).then_some(total),
        reporters,
        eligible,
        overflowed,
        agents: rows,
        groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_protocol::{rpc::Quota, ContextUsage};
    use serde_json::json;

    fn agent(id: &str, usage: Option<Usage>) -> Agent {
        let mut agent: Agent = serde_json::from_value(json!({
            "id":id,"name":id,"role":"","status":"idle","runtime":"codex",
            "workdir":"","backend":"pty","last_activity_at":0,
        }))
        .unwrap();
        agent.usage = usage;
        agent
    }

    #[test]
    fn a_silent_report_and_a_reported_zero_remain_distinct() {
        let absent = usage_data(&[agent("silent", None)], None, &BTreeMap::new(), 0);
        assert_eq!(absent.total, None);
        assert_eq!((absent.reporters, absent.eligible), (0, 1));
        assert_eq!(absent.agents[0].context, None);
        let zero = usage_data(
            &[agent("zero", Some(Usage::default())), agent("silent", None)],
            None,
            &BTreeMap::new(),
            0,
        );
        assert_eq!(zero.total, Some(Usage::default()));
        assert_eq!((zero.reporters, zero.eligible), (1, 2));
        assert_eq!(zero.agents[1].usage, None);
    }

    #[test]
    fn all_categories_and_turns_preserve_large_exact_counters_and_signal_overflow() {
        let first = Usage {
            input_tokens: u64::MAX,
            output_tokens: 1,
            cache_read_tokens: 2,
            cache_creation_tokens: 3,
            turns: 4,
        };
        let second = Usage {
            input_tokens: 2,
            output_tokens: 3,
            cache_read_tokens: 4,
            cache_creation_tokens: 5,
            turns: 6,
        };
        let data = usage_data(
            &[agent("first", Some(first)), agent("second", Some(second))],
            None,
            &BTreeMap::new(),
            0,
        );
        assert_eq!(data.total, None);
        assert!(data.overflowed);
        assert_eq!(
            data.agents[0].usage.as_ref().unwrap().input_tokens,
            u64::MAX
        );
    }

    #[test]
    fn supplied_context_samples_and_quota_freshness_do_not_invent_reports() {
        let mut member = agent("agent", None);
        member.context = Some(ContextUsage::new(0, 200_000, 0));
        let samples = BTreeMap::from([(
            "agent".into(),
            VecDeque::from([ContextPoint {
                at_ms: 50,
                percent: 0,
            }]),
        )]);
        let session = SessionInfo {
            quotas: vec![
                Quota {
                    runtime: RuntimeKind::Codex,
                    window: "5h".into(),
                    percent_used: 0.0,
                    resets_at: None,
                    updated_at: 0,
                },
                Quota {
                    runtime: RuntimeKind::Codex,
                    window: "week".into(),
                    percent_used: 80.0,
                    resets_at: Some(2_000_000),
                    updated_at: -1,
                },
            ],
            ..SessionInfo::default()
        };
        let data = usage_data(&[member], Some(&session), &samples, 1_200_000);
        assert_eq!(data.total, None);
        assert_eq!(data.agents[0].context.as_ref().unwrap().tokens, 0);
        assert_eq!(
            data.agents[0].samples,
            [ContextPoint {
                at_ms: 50,
                percent: 0
            }]
        );
        assert!(!data.groups[0].quotas[0].stale);
        assert!(data.groups[0].quotas[1].stale);
        assert_eq!(data.groups[0].quotas[0].quota.resets_at, None);
    }
}
