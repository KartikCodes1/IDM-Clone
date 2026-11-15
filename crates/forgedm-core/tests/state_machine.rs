//! S1.1 state machine tests. The expected edge tables below are written out from
//! PHASE_0_1_PLAN Appendix A.6 (including PCR-01), independently of the implementation.

use forgedm_core::state::{
    Command, PartFileInvalidOutcome, Transition, TransitionError, on_startup,
    part_file_invalid_outcome, transition,
};
use forgedm_protocol::{DownloadState as S, ErrorCode};
use proptest::prelude::*;

const PAUSE: Command = Command::Pause {
    resumable: true,
    discard_progress: false,
};

/// Every legal state-changing edge of A.6: (from, command, to).
fn legal_edges() -> Vec<(S, Command, S)> {
    use Command as C;
    vec![
        // queued
        (S::Queued, C::Admit, S::Probing),
        (S::Queued, PAUSE, S::Paused),
        (S::Queued, C::Cancel, S::Cancelled),
        (S::Queued, C::AdmitCommitted, S::Verifying),
        // probing
        (S::Probing, C::ProbeSucceeded, S::Downloading),
        (S::Probing, C::ZeroLength, S::Verifying),
        (S::Probing, C::WaitRetry, S::RetryWaiting),
        (S::Probing, C::Fail, S::Failed),
        (S::Probing, C::NeedCredentials, S::NeedsCredentials),
        (S::Probing, C::NeedUrlRefresh, S::NeedsUrlRefresh),
        (S::Probing, PAUSE, S::Pausing),
        (S::Probing, C::Cancel, S::Cancelled),
        (S::Probing, C::Interrupt, S::Interrupted),
        // downloading
        (S::Downloading, PAUSE, S::Pausing),
        (S::Downloading, C::TransferComplete, S::Verifying),
        (S::Downloading, C::WaitRetry, S::RetryWaiting),
        (S::Downloading, C::Fail, S::Failed),
        (S::Downloading, C::NeedCredentials, S::NeedsCredentials),
        (S::Downloading, C::NeedUrlRefresh, S::NeedsUrlRefresh),
        (S::Downloading, C::PauseForDisk, S::Paused),
        (S::Downloading, C::Cancel, S::Cancelled),
        (S::Downloading, C::Interrupt, S::Interrupted),
        // pausing
        (S::Pausing, C::Quiesced, S::Paused),
        (S::Pausing, C::Cancel, S::Cancelled),
        (S::Pausing, C::Interrupt, S::Interrupted),
        // paused
        (S::Paused, C::Resume, S::Queued),
        (S::Paused, C::Cancel, S::Cancelled),
        // retry_waiting
        (S::RetryWaiting, C::TimerDue, S::Queued),
        (S::RetryWaiting, C::Resume, S::Queued),
        (S::RetryWaiting, PAUSE, S::Paused),
        (S::RetryWaiting, C::Cancel, S::Cancelled),
        (S::RetryWaiting, C::Fail, S::Failed),
        // verifying
        (S::Verifying, C::Verified, S::Finalizing),
        (S::Verifying, C::Fail, S::Failed),
        (S::Verifying, C::Cancel, S::Cancelled),
        (S::Verifying, C::Interrupt, S::Interrupted),
        // finalizing (PCR-01: only completed or failed)
        (S::Finalizing, C::Complete, S::Completed),
        (S::Finalizing, C::Fail, S::Failed),
        // failed, cancelled
        (S::Failed, C::Retry, S::Queued),
        (S::Cancelled, C::Retry, S::Queued),
        // needs_*
        (S::NeedsCredentials, C::Retry, S::Queued),
        (S::NeedsCredentials, C::Cancel, S::Cancelled),
        (S::NeedsUrlRefresh, C::Retry, S::Queued),
        (S::NeedsUrlRefresh, C::Cancel, S::Cancelled),
        // interrupted
        (S::Interrupted, C::AutoResume, S::Queued),
        (S::Interrupted, C::Resume, S::Queued),
        (S::Interrupted, C::Retry, S::Queued),
        (S::Interrupted, PAUSE, S::Paused),
        (S::Interrupted, C::Cancel, S::Cancelled),
    ]
}

/// States where a repeated Resume/Retry is a no-op (queued or in flight).
const IN_FLIGHT: [S; 6] = [
    S::Queued,
    S::Probing,
    S::Downloading,
    S::Pausing,
    S::Verifying,
    S::Finalizing,
];

/// Idempotent no-ops: (from, command).
fn noop_edges() -> Vec<(S, Command)> {
    let mut out = vec![
        (S::Paused, PAUSE),
        (S::Pausing, PAUSE),
        (S::Cancelled, Command::Cancel),
    ];
    for state in IN_FLIGHT {
        out.push((state, Command::Resume));
        out.push((state, Command::Retry));
    }
    out
}

fn all_commands() -> Vec<Command> {
    use Command as C;
    let mut cmds = vec![
        C::Resume,
        C::Cancel,
        C::Retry,
        C::Admit,
        C::AdmitCommitted,
        C::ProbeSucceeded,
        C::ZeroLength,
        C::TransferComplete,
        C::WaitRetry,
        C::Fail,
        C::NeedCredentials,
        C::NeedUrlRefresh,
        C::PauseForDisk,
        C::Quiesced,
        C::TimerDue,
        C::Interrupt,
        C::AutoResume,
        C::Verified,
        C::Complete,
    ];
    for resumable in [true, false] {
        for discard_progress in [true, false] {
            cmds.push(C::Pause {
                resumable,
                discard_progress,
            });
        }
    }
    cmds
}

fn is_active(state: S) -> bool {
    matches!(state, S::Probing | S::Downloading)
}

#[test]
fn legal_transition_table() {
    let edges = legal_edges();
    let noops = noop_edges();
    let mut checked = 0;
    for &from in S::ALL {
        for cmd in all_commands() {
            let result = transition(from, cmd);
            let refuses_pause = matches!(
                cmd,
                Command::Pause {
                    resumable: false,
                    discard_progress: false
                }
            ) && is_active(from);
            if refuses_pause {
                assert_eq!(
                    result,
                    Err(TransitionError::NotResumable),
                    "{from:?} {cmd:?}"
                );
                continue;
            }
            // A Pause with resumable=false behaves like the table entry otherwise.
            let key = if matches!(cmd, Command::Pause { .. }) {
                PAUSE
            } else {
                cmd
            };
            if let Some((_, _, to)) = edges.iter().find(|(f, c, _)| *f == from && *c == key) {
                assert!(
                    result.is_ok(),
                    "{from:?} {cmd:?} should be legal: {result:?}"
                );
                let t = result.unwrap();
                assert_eq!((t.to, t.changed), (*to, true), "{from:?} {cmd:?}");
                checked += 1;
            } else if noops.iter().any(|(f, c)| *f == from && *c == key) {
                assert!(result.is_ok(), "{from:?} {cmd:?} should no-op: {result:?}");
                let t = result.unwrap();
                assert_eq!(
                    (t.to, t.changed, t.discard_progress),
                    (from, false, false),
                    "{from:?} {cmd:?}"
                );
                checked += 1;
            } else {
                assert_eq!(
                    result,
                    Err(TransitionError::InvalidState {
                        current: from,
                        command: cmd
                    }),
                    "{from:?} {cmd:?} must be invalid_state"
                );
            }
        }
    }
    // Pause expands to several argument variants, so every table row is hit at least once.
    assert!(
        checked >= edges.len() + noops.len(),
        "only {checked} of {} table rows exercised",
        edges.len() + noops.len()
    );
    // Every table entry names a real, distinct state pair.
    for (from, _, to) in &edges {
        assert_ne!(from, to);
    }
    let mut unique: Vec<_> = edges.iter().map(|(f, c, _)| (*f, *c)).collect();
    unique.dedup();
    assert_eq!(unique.len(), edges.len(), "duplicate (state, command) rows");
}

#[test]
fn illegal_transition_typed_error() {
    let err = transition(S::Completed, Command::Resume).unwrap_err();
    assert_eq!(
        err,
        TransitionError::InvalidState {
            current: S::Completed,
            command: Command::Resume
        }
    );
    assert_eq!(err.error_code(), ErrorCode::InvalidState);
    assert!(err.to_string().contains("completed"), "{err}");

    // Cancel is refused in finalizing and completed, and on failed (terminal, Retry only).
    for state in [S::Finalizing, S::Completed, S::Failed] {
        assert_eq!(
            transition(state, Command::Cancel),
            Err(TransitionError::InvalidState {
                current: state,
                command: Command::Cancel
            })
        );
    }
    // Resume on a terminal state says "use Retry"; Retry on completed is refused.
    for state in [
        S::Completed,
        S::Failed,
        S::Cancelled,
        S::NeedsCredentials,
        S::NeedsUrlRefresh,
    ] {
        assert!(transition(state, Command::Resume).is_err(), "{state:?}");
    }
    assert!(transition(S::Completed, Command::Retry).is_err());
    // A state from a newer peer is never transitionable.
    for cmd in all_commands() {
        assert!(transition(S::Unknown, cmd).is_err(), "{cmd:?}");
    }
    let not_resumable = TransitionError::NotResumable;
    assert_eq!(not_resumable.error_code(), ErrorCode::NotResumable);
}

#[test]
fn finalizing_never_requeues_pcr01() {
    // A finalizing download exits only to completed or failed (A.6, PCR-01).
    for cmd in all_commands() {
        match transition(S::Finalizing, cmd) {
            Ok(t) if t.changed => assert!(matches!(t.to, S::Completed | S::Failed), "{cmd:?}"),
            Ok(t) => assert_eq!(t.to, S::Finalizing),
            Err(_) => {}
        }
    }
    assert!(!transition(S::Finalizing, Command::Retry).unwrap().changed);
    assert!(transition(S::Finalizing, Command::AutoResume).is_err());
}

#[test]
fn pause_resume_cancel_idempotent() {
    // Pause twice: the second is a no-op that reports the same state.
    let first = transition(S::Downloading, PAUSE).unwrap();
    assert_eq!(
        first,
        Transition {
            to: S::Pausing,
            changed: true,
            discard_progress: false
        }
    );
    let again = transition(first.to, PAUSE).unwrap();
    assert_eq!(
        again,
        Transition {
            to: S::Pausing,
            changed: false,
            discard_progress: false
        }
    );
    let paused = transition(S::Pausing, Command::Quiesced).unwrap();
    assert_eq!(paused.to, S::Paused);
    assert!(!transition(S::Paused, PAUSE).unwrap().changed);

    // Resume twice.
    let queued = transition(S::Paused, Command::Resume).unwrap();
    assert_eq!((queued.to, queued.changed), (S::Queued, true));
    assert!(!transition(S::Queued, Command::Resume).unwrap().changed);
    for active in [S::Probing, S::Downloading] {
        assert_eq!(transition(active, Command::Resume).unwrap().to, active);
        assert!(!transition(active, Command::Resume).unwrap().changed);
    }
    // Resume from retry_waiting means "retry now".
    assert_eq!(
        transition(S::RetryWaiting, Command::Resume).unwrap().to,
        S::Queued
    );

    // Cancel twice.
    let cancelled = transition(S::Downloading, Command::Cancel).unwrap();
    assert_eq!((cancelled.to, cancelled.changed), (S::Cancelled, true));
    let again = transition(S::Cancelled, Command::Cancel).unwrap();
    assert_eq!((again.to, again.changed), (S::Cancelled, false));

    // Retry twice.
    let retried = transition(S::Failed, Command::Retry).unwrap();
    assert_eq!((retried.to, retried.changed), (S::Queued, true));
    assert!(!transition(S::Queued, Command::Retry).unwrap().changed);
    assert!(!transition(S::Downloading, Command::Retry).unwrap().changed);
}

#[test]
fn pause_nonresumable_requires_discard_decision() {
    let refuse = Command::Pause {
        resumable: false,
        discard_progress: false,
    };
    let discard = Command::Pause {
        resumable: false,
        discard_progress: true,
    };
    for active in [S::Probing, S::Downloading] {
        assert_eq!(
            transition(active, refuse),
            Err(TransitionError::NotResumable),
            "{active:?}"
        );
        let t = transition(active, discard).unwrap();
        assert_eq!(
            t,
            Transition {
                to: S::Pausing,
                changed: true,
                discard_progress: true
            }
        );
    }
    // Resumable downloads never need the decision, and discard is not recorded for them.
    let t = transition(
        S::Downloading,
        Command::Pause {
            resumable: true,
            discard_progress: true,
        },
    )
    .unwrap();
    assert_eq!(
        t,
        Transition {
            to: S::Pausing,
            changed: true,
            discard_progress: false
        }
    );
    // The decision only applies to active downloads: idle ones just pause.
    for idle in [S::Queued, S::RetryWaiting, S::Interrupted] {
        let t = transition(idle, refuse).unwrap();
        assert_eq!((t.to, t.discard_progress), (S::Paused, false), "{idle:?}");
    }
    // Already pausing/paused is a no-op even without a decision.
    assert!(!transition(S::Pausing, refuse).unwrap().changed);
    assert!(!transition(S::Paused, refuse).unwrap().changed);
}

fn arb_command() -> impl Strategy<Value = Command> {
    proptest::sample::select(all_commands())
}

proptest! {
    #[test]
    fn proptest_terminal_states_exit_only_via_retry(
        start in proptest::sample::select(S::ALL.to_vec()),
        cmds in proptest::collection::vec(arb_command(), 1..40),
    ) {
        let mut state = start;
        for cmd in cmds {
            let Ok(t) = transition(state, cmd) else { continue };
            if state.is_terminal() && t.changed {
                prop_assert!(matches!(state, S::Failed | S::Cancelled), "{state:?}");
                prop_assert_eq!(cmd, Command::Retry);
                prop_assert_eq!(t.to, S::Queued);
            }
            // completed never moves at all.
            if state == S::Completed {
                prop_assert!(!t.changed);
            }
            // The reply is always the resulting state; a no-op reports the same state.
            if !t.changed {
                prop_assert_eq!(t.to, state);
            }
            state = t.to;
        }
    }
}

#[test]
fn startup_reconcile_mapping() {
    // ARCHITECTURE 6.3 step 1.
    for (from, to) in [
        (S::Probing, S::Interrupted),
        (S::Downloading, S::Interrupted),
        (S::Verifying, S::Interrupted),
        (S::Pausing, S::Paused),
        (S::RetryWaiting, S::RetryWaiting),
        // finalizing is reconciled against the part file, not here (PCR-01).
        (S::Finalizing, S::Finalizing),
        (S::Queued, S::Queued),
        (S::Paused, S::Paused),
        (S::Completed, S::Completed),
        (S::Failed, S::Failed),
        (S::Cancelled, S::Cancelled),
        (S::Interrupted, S::Interrupted),
        (S::NeedsCredentials, S::NeedsCredentials),
        (S::NeedsUrlRefresh, S::NeedsUrlRefresh),
    ] {
        assert_eq!(on_startup(from), to, "{from:?}");
    }
    assert_eq!(on_startup(S::Unknown), S::Unknown);
    // Every state is covered and the mapping is stable (idempotent).
    for &state in S::ALL {
        assert_eq!(
            on_startup(on_startup(state)),
            on_startup(state),
            "{state:?}"
        );
    }
    // The startup mapping never produces a state the machine could not reach by an edge
    // or by shutdown: probing/downloading/verifying -> interrupted, pausing -> paused.
    for &state in S::ALL {
        let mapped = on_startup(state);
        if mapped != state {
            let edge_exists = legal_edges()
                .iter()
                .any(|(f, _, t)| *f == state && *t == mapped);
            assert!(edge_exists, "{state:?} -> {mapped:?} is not an A.6 edge");
        }
    }
}

#[test]
fn part_file_invalid_outcome_follows_pcr01() {
    // Before finalizing: segments reset and the download restarts automatically.
    for state in [
        S::Queued,
        S::Probing,
        S::Downloading,
        S::Interrupted,
        S::Paused,
        S::Verifying,
    ] {
        assert_eq!(
            part_file_invalid_outcome(state),
            PartFileInvalidOutcome::ResetAndRequeue,
            "{state:?}"
        );
    }
    // In finalizing: failed(part_file_invalid); the user's Retry restarts from 0.
    assert_eq!(
        part_file_invalid_outcome(S::Finalizing),
        PartFileInvalidOutcome::FailPartFileInvalid
    );
    assert_eq!(
        transition(S::Finalizing, Command::Fail).unwrap().to,
        S::Failed
    );
    assert_eq!(transition(S::Failed, Command::Retry).unwrap().to, S::Queued);
}
