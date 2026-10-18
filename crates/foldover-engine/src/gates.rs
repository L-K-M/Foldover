const MIN_INTERACTIVE_DELETIONS: u64 = 50;
const PERCENT_SCALE: u128 = 100;
const INTERACTIVE_DELETION_PERCENT: u128 = 1;
const MAX_UNATTENDED_DELETION_PERCENT: u128 = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunContext {
    Interactive,
    Unattended,
}

/// This is one review requirement, never permission to execute a plan. Root,
/// identity, canary, space, freshness and first-run gates must also pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeletionReview {
    WithinLimits,
    ConfirmationRequired,
    StopUnattended,
}

/// Counts from one complete target snapshot, before applying a plan. Evaluate
/// targets separately; source-only/new paths must not dilute the denominator.
/// Replacements are not deletions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeletionCounts {
    paths: u64,
    deletions: u64,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("Deletion count {deletions} exceeds analyzed path count {paths}")]
pub struct InvalidDeletionCounts {
    paths: u64,
    deletions: u64,
}

impl DeletionCounts {
    pub fn new(paths: u64, deletions: u64) -> Result<Self, InvalidDeletionCounts> {
        if deletions > paths {
            return Err(InvalidDeletionCounts { paths, deletions });
        }
        Ok(Self { paths, deletions })
    }

    pub fn paths(self) -> u64 {
        self.paths
    }

    pub fn deletions(self) -> u64 {
        self.deletions
    }

    /// PLAN.md §3.6 uses separate interactive and unattended thresholds.
    /// Confirmations must later be bound to the exact reviewed plan, not cached
    /// as a pair-level override.
    pub fn review(self, context: RunContext) -> DeletionReview {
        let deletions = u128::from(self.deletions);
        let paths = u128::from(self.paths);
        match context {
            RunContext::Interactive
                if self.deletions > MIN_INTERACTIVE_DELETIONS
                    && deletions * PERCENT_SCALE > paths * INTERACTIVE_DELETION_PERCENT =>
            {
                DeletionReview::ConfirmationRequired
            }
            RunContext::Unattended
                if deletions * PERCENT_SCALE > paths * MAX_UNATTENDED_DELETION_PERCENT =>
            {
                DeletionReview::StopUnattended
            }
            _ => DeletionReview::WithinLimits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(paths: u64, deletions: u64, context: RunContext) -> DeletionReview {
        DeletionCounts::new(paths, deletions)
            .unwrap()
            .review(context)
    }

    #[test]
    fn interactive_review_uses_the_higher_threshold_with_strict_boundaries() {
        for (paths, deletions, expected) in [
            (0, 0, DeletionReview::WithinLimits),
            (50, 50, DeletionReview::WithinLimits),
            (51, 51, DeletionReview::ConfirmationRequired),
            (5_000, 50, DeletionReview::WithinLimits),
            (5_000, 51, DeletionReview::ConfirmationRequired),
            (5_099, 50, DeletionReview::WithinLimits),
            (5_099, 51, DeletionReview::ConfirmationRequired),
            (5_100, 51, DeletionReview::WithinLimits),
            (10_000, 100, DeletionReview::WithinLimits),
            (10_000, 101, DeletionReview::ConfirmationRequired),
        ] {
            assert_eq!(review(paths, deletions, RunContext::Interactive), expected);
        }
    }

    #[test]
    fn unattended_runs_stop_above_half_even_for_small_pairs() {
        for (paths, deletions, expected) in [
            (0, 0, DeletionReview::WithinLimits),
            (1, 1, DeletionReview::StopUnattended),
            (2, 1, DeletionReview::WithinLimits),
            (3, 2, DeletionReview::StopUnattended),
            (1_000, 51, DeletionReview::WithinLimits),
            (10_000, 5_000, DeletionReview::WithinLimits),
            (10_000, 5_001, DeletionReview::StopUnattended),
        ] {
            assert_eq!(review(paths, deletions, RunContext::Unattended), expected);
        }
    }

    #[test]
    fn invalid_counts_cannot_bypass_the_fuse() {
        for (paths, deletions) in [(0, 1), (50, 51), (u64::MAX - 1, u64::MAX)] {
            assert_eq!(
                DeletionCounts::new(paths, deletions),
                Err(InvalidDeletionCounts { paths, deletions })
            );
        }
    }

    #[test]
    fn maximum_counts_keep_exact_fractional_boundaries_without_overflow() {
        let paths = u64::MAX;
        let one_percent_floor = paths / 100;
        assert_eq!(
            review(paths, one_percent_floor, RunContext::Interactive),
            DeletionReview::WithinLimits
        );
        assert_eq!(
            review(paths, one_percent_floor + 1, RunContext::Interactive),
            DeletionReview::ConfirmationRequired
        );
        assert_eq!(
            review(paths, paths / 2, RunContext::Unattended),
            DeletionReview::WithinLimits
        );
        assert_eq!(
            review(paths, paths / 2 + 1, RunContext::Unattended),
            DeletionReview::StopUnattended
        );
    }

    #[test]
    fn finite_counts_match_fractional_policy_and_never_reopen_after_tripping() {
        for paths in 0..=300 {
            for context in [RunContext::Interactive, RunContext::Unattended] {
                let mut tripped = false;
                for deletions in 0..=paths {
                    let actual = review(paths, deletions, context);
                    let fraction = deletions as f64 / paths as f64;
                    let expected = match context {
                        RunContext::Interactive if deletions > 50 && fraction > 0.01 => {
                            DeletionReview::ConfirmationRequired
                        }
                        RunContext::Unattended if fraction > 0.5 => DeletionReview::StopUnattended,
                        _ => DeletionReview::WithinLimits,
                    };
                    assert_eq!(
                        actual, expected,
                        "paths={paths}, deletions={deletions}, context={context:?}"
                    );
                    assert!(!tripped || actual != DeletionReview::WithinLimits);
                    tripped |= actual != DeletionReview::WithinLimits;
                }
            }
        }
    }
}
