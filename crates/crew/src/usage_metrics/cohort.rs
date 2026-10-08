//! Cost per completed task, over the tasks first completed in a period.
//!
//! The cohort is fixed by each task's first transition to Done; a reopened
//! task counts once. Its cost is the task-bound live cost observed from the
//! start of the run up to that first completion, which can predate the
//! period. A cohort whose membership is not fully known has no figure at all,
//! because a lower bound needs the whole cohort.

use crew_core::rpc::features::{Id, List, Timestamp, Wide};
use crew_core::rpc::usage_metrics::{
    CompletionCohort, CostBasis, CostEstimate, CoverageExclusion, CoverageReason, EstimateState,
    MetricCoverage,
};

use super::price::{Book, Bounds};

/// One task first completed in the period.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub task_id: String,
    /// Its observed cost up to the first completion; `None` when no cost was
    /// observed for it. An observed zero is `Some`.
    pub cost: Option<Bounds>,
    /// Part of that cost was spent before the period began.
    pub earlier_spend: bool,
}

fn unavailable(book: &Book, reason: CoverageReason, coverage: MetricCoverage) -> CostEstimate {
    CostEstimate {
        price_book_id: Id::new(book.id.clone()).ok(),
        prices_checked_at: Timestamp::new(book.checked_at_ms).ok(),
        basis: CostBasis::StandardApiEquivalent,
        lower_pico_usd: None,
        upper_pico_usd: None,
        state: EstimateState::Unavailable,
        reason: Some(reason),
        assumptions: List::default(),
        excluded_models: List::default(),
        coverage,
    }
}

fn coverage(members: u64, costed: u64) -> MetricCoverage {
    let missing = members - costed;
    MetricCoverage {
        eligible_observations: Wide(costed as u128),
        observations_seen: Wide(members as u128),
        exclusions: List::new(if missing > 0 {
            vec![CoverageExclusion {
                reason: CoverageReason::NoEligibleCost,
                observations: Wide(missing as u128),
            }]
        } else {
            Vec::new()
        })
        .expect("one exclusion"),
        ..MetricCoverage::default()
    }
}

/// The completed-task cohort and its cost per task. `membership_complete` is
/// false when some first completions in the period are not recorded.
pub fn cohort(book: &Book, members: &[Member], membership_complete: bool) -> CompletionCohort {
    cohort_with(book, members, membership_complete, None)
}

/// [`cohort`], where `spend_gap` says why some of the members' spend may be
/// missing even when each has observed cost (unattributed work or a source
/// gap in the run): the upper bound is then unknown.
pub fn cohort_with(
    book: &Book,
    members: &[Member],
    membership_complete: bool,
    spend_gap: Option<CoverageReason>,
) -> CompletionCohort {
    let n = members.len() as u64;
    let costed = members.iter().filter(|m| m.cost.is_some()).count() as u64;
    // How many first completions are missing is unknown; `membership_complete`
    // says that some are.
    let membership_coverage = MetricCoverage {
        eligible_observations: Wide(n as u128),
        observations_seen: Wide(n as u128),
        ..MetricCoverage::default()
    };
    let cost_coverage = coverage(n, costed);
    let cost_per_completed_task = if !membership_complete {
        unavailable(
            book,
            CoverageReason::CompletionHistoryIncomplete,
            cost_coverage,
        )
    } else if n == 0 {
        unavailable(book, CoverageReason::NoCompletions, cost_coverage)
    } else if costed == 0 {
        unavailable(book, CoverageReason::NoEligibleCost, cost_coverage)
    } else {
        // Checked: an overflowing sum claims no bound at all.
        let mut total = Some(Bounds { lo: 0, hi: Some(0) });
        for member in members {
            total = total.and_then(|t| match member.cost {
                Some(cost) => t.checked_plus(cost),
                None => Some(Bounds { lo: t.lo, hi: None }),
            });
        }
        let Some(Bounds { lo, mut hi }) = total else {
            return CompletionCohort {
                first_completions: Wide(n as u128),
                membership_complete,
                tasks_with_observed_cost: Wide(costed as u128),
                membership_coverage,
                cost_per_completed_task: unavailable(
                    book,
                    CoverageReason::InvalidCounter,
                    cost_coverage,
                ),
                includes_earlier_spend: members.iter().any(|m| m.earlier_spend && m.cost.is_some()),
            };
        };
        if spend_gap.is_some() {
            hi = None;
        }
        let n = n as u128;
        let (state, reason) = match hi {
            Some(_) => (EstimateState::Finite, None),
            None if costed < n as u64 => {
                (EstimateState::Partial, Some(CoverageReason::NoEligibleCost))
            }
            None => (
                EstimateState::Partial,
                spend_gap.or(Some(CoverageReason::NotReported)),
            ),
        };
        CostEstimate {
            price_book_id: Id::new(book.id.clone()).ok(),
            prices_checked_at: Timestamp::new(book.checked_at_ms).ok(),
            basis: CostBasis::StandardApiEquivalent,
            // A lower bound rounds down, an upper bound up.
            lower_pico_usd: Some(Wide(lo / n)),
            upper_pico_usd: hi.map(|h| Wide(h.div_ceil(n))),
            state,
            reason,
            assumptions: List::default(),
            excluded_models: List::default(),
            coverage: cost_coverage,
        }
    };
    CompletionCohort {
        first_completions: Wide(n as u128),
        membership_complete,
        tasks_with_observed_cost: Wide(costed as u128),
        membership_coverage,
        cost_per_completed_task,
        includes_earlier_spend: members.iter().any(|m| m.earlier_spend && m.cost.is_some()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, cost: Option<(u128, Option<u128>)>) -> Member {
        Member {
            task_id: id.into(),
            cost: cost.map(|(lo, hi)| Bounds { lo, hi }),
            earlier_spend: false,
        }
    }

    #[test]
    fn full_cohort_divides_both_bounds_rounding_outward() {
        let book = Book::standard_2026_10_08();
        let c = cohort(
            &book,
            &[
                member("a", Some((10, Some(11)))),
                member("b", Some((0, Some(0)))),
                member("c", Some((1, Some(1)))),
            ],
            true,
        );
        let e = &c.cost_per_completed_task;
        assert_eq!(e.state, EstimateState::Finite);
        assert_eq!(e.lower_pico_usd, Some(Wide(3)), "11 / 3 rounds down");
        assert_eq!(e.upper_pico_usd, Some(Wide(4)), "12 / 3 rounds up");
        assert_eq!(c.first_completions, Wide(3));
        assert_eq!(
            c.tasks_with_observed_cost,
            Wide(3),
            "an observed zero is observed"
        );
    }

    #[test]
    fn missing_task_cost_gives_a_lower_bound_over_the_whole_cohort() {
        let book = Book::standard_2026_10_08();
        let c = cohort(
            &book,
            &[member("a", Some((10, Some(10)))), member("b", None)],
            true,
        );
        let e = &c.cost_per_completed_task;
        assert_eq!(e.state, EstimateState::Partial);
        assert_eq!(e.lower_pico_usd, Some(Wide(5)));
        assert_eq!(e.upper_pico_usd, None);
        assert_eq!(e.coverage.eligible_observations, Wide(1));
        assert_eq!(e.coverage.observations_seen, Wide(2));
    }

    #[test]
    fn incomplete_membership_and_empty_cohorts_are_unavailable() {
        let book = Book::standard_2026_10_08();
        let incomplete = cohort(&book, &[member("a", Some((10, Some(10))))], false);
        assert_eq!(
            incomplete.cost_per_completed_task.state,
            EstimateState::Unavailable
        );
        assert_eq!(
            incomplete.cost_per_completed_task.reason,
            Some(CoverageReason::CompletionHistoryIncomplete)
        );
        assert_eq!(
            incomplete.cost_per_completed_task.lower_pico_usd, None,
            "no lower bound without the whole cohort"
        );
        let empty = cohort(&book, &[], true);
        assert_eq!(
            empty.cost_per_completed_task.reason,
            Some(CoverageReason::NoCompletions)
        );
        let uncosted = cohort(&book, &[member("a", None)], true);
        assert_eq!(
            uncosted.cost_per_completed_task.reason,
            Some(CoverageReason::NoEligibleCost)
        );
    }

    #[test]
    fn earlier_spend_is_reported() {
        let book = Book::standard_2026_10_08();
        let mut m = member("a", Some((1, Some(1))));
        m.earlier_spend = true;
        assert!(cohort(&book, &[m], true).includes_earlier_spend);
    }

    #[test]
    fn cohort_sums_never_wrap_or_panic() {
        let book = Book::standard_2026_10_08();
        let c = cohort(
            &book,
            &[
                member("a", Some((u128::MAX, Some(u128::MAX)))),
                member("b", Some((1, Some(1)))),
            ],
            true,
        );
        let e = &c.cost_per_completed_task;
        assert_eq!(e.state, EstimateState::Unavailable);
        assert_eq!(e.reason, Some(CoverageReason::InvalidCounter));
        assert_eq!(e.lower_pico_usd, None, "no clamped bound");
        assert_eq!(e.upper_pico_usd, None);
        // An unknown upper bound does not hide the overflow.
        let c = cohort(
            &book,
            &[
                member("a", Some((u128::MAX, None))),
                member("b", Some((1, Some(1)))),
            ],
            true,
        );
        assert_eq!(
            c.cost_per_completed_task.reason,
            Some(CoverageReason::InvalidCounter)
        );
        assert_eq!(c.cost_per_completed_task.lower_pico_usd, None);
    }

    #[test]
    fn possibly_missing_spend_leaves_the_upper_bound_open() {
        let book = Book::standard_2026_10_08();
        let members = [member("a", Some((10, Some(10))))];
        let c = cohort_with(
            &book,
            &members,
            true,
            Some(CoverageReason::UnknownAttribution),
        );
        let e = &c.cost_per_completed_task;
        assert_eq!(e.state, EstimateState::Partial);
        assert_eq!(e.reason, Some(CoverageReason::UnknownAttribution));
        assert_eq!(e.lower_pico_usd, Some(Wide(10)));
        assert_eq!(e.upper_pico_usd, None);
    }
}
