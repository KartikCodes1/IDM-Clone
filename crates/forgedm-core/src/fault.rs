//! Fault points: places where a crash test may stop the process (PLAN S1.14).
//!
//! The engine calls [`hit`] at the five moments that matter for crash recovery. Without an
//! installed hook that is one atomic load and nothing else. Only the agent installs a hook,
//! and only when it is compiled with its `fault-injection` feature (never in release), so
//! core itself has no switch that reads the environment.

use std::sync::OnceLock;

/// A moment of the download life cycle where a crash is interesting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultPoint {
    /// The first checkpoint of a run was committed to the database.
    AfterFirstCommit,
    /// Bytes were flushed (`sync_data`) but the checkpoint was not committed yet.
    AfterFlushBeforeCommit,
    /// A segment was split in memory; the new segment is not persisted yet.
    MidSplit,
    /// `Zone.Identifier` is on the `.part`; the rename has not happened.
    FinalizingBeforeRename,
    /// The `.part` was renamed to its final name; the database still says `finalizing`.
    AfterRenameBeforeCompleted,
}

impl FaultPoint {
    /// Every point, in life-cycle order.
    pub const ALL: [Self; 5] = [
        Self::AfterFirstCommit,
        Self::AfterFlushBeforeCommit,
        Self::MidSplit,
        Self::FinalizingBeforeRename,
        Self::AfterRenameBeforeCompleted,
    ];

    /// The name used in `FORGEDM_FAULT`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::AfterFirstCommit => "after_first_commit",
            Self::AfterFlushBeforeCommit => "after_flush_before_commit",
            Self::MidSplit => "mid_split",
            Self::FinalizingBeforeRename => "finalizing_before_rename",
            Self::AfterRenameBeforeCompleted => "after_rename_before_completed",
        }
    }

    /// Parses a `FORGEDM_FAULT` value.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

type Hook = Box<dyn Fn(FaultPoint) + Send + Sync>;

static HOOK: OnceLock<Hook> = OnceLock::new();

/// Installs the process-wide hook. Returns `false` when one is already installed.
pub fn install(hook: impl Fn(FaultPoint) + Send + Sync + 'static) -> bool {
    HOOK.set(Box::new(hook)).is_ok()
}

/// Reports that execution reached `point`. A no-op unless a hook is installed.
#[inline]
pub fn hit(point: FaultPoint) {
    if let Some(hook) = HOOK.get() {
        hook(point);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_point_round_trips_through_its_name() {
        for point in FaultPoint::ALL {
            assert_eq!(FaultPoint::parse(point.name()), Some(point));
        }
        assert_eq!(FaultPoint::parse("nope"), None);
        assert_eq!(FaultPoint::parse(""), None);
    }

    #[test]
    fn hit_without_a_hook_does_nothing() {
        hit(FaultPoint::MidSplit);
    }
}
