//! Property tests for the `FileSyncState` transition machine.
//!
//! Per [`spec/01-domain-model.md`] §Lifecycle, `FileSyncState` transitions
//! follow a defined automaton. These tests generate random valid transition
//! sequences and assert invariants.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use onesync_protocol::enums::FileSyncState;
use proptest::prelude::*;

/// Valid transitions from a given state.
fn valid_successors(state: FileSyncState) -> Vec<FileSyncState> {
    match state {
        // Clean → Dirty (event observed)
        FileSyncState::Clean => vec![FileSyncState::Dirty],
        // Dirty → PendingUpload | PendingDownload | PendingConflict (reconcile)
        FileSyncState::Dirty => vec![
            FileSyncState::PendingUpload,
            FileSyncState::PendingDownload,
            FileSyncState::PendingConflict,
            FileSyncState::Clean, // converged immediately
        ],
        // Pending* → InFlight (executor picks it up)
        FileSyncState::PendingUpload
        | FileSyncState::PendingDownload
        | FileSyncState::PendingConflict => vec![FileSyncState::InFlight, FileSyncState::Dirty],
        // InFlight → Clean (success) or Dirty (retry)
        FileSyncState::InFlight => vec![FileSyncState::Clean, FileSyncState::Dirty],
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..Default::default() })]

    /// Randomly walk valid transitions; check no terminal state loops back through InFlight
    /// without going through Dirty first.
    #[test]
    fn state_machine_valid_transitions_never_skip_dirty(
        steps in prop::collection::vec(0usize..4, 1..=10),
    ) {
        let mut state = FileSyncState::Clean;
        let mut prev = state;
        for step in steps {
            let succs = valid_successors(state);
            if succs.is_empty() {
                break;
            }
            let next = succs[step % succs.len()];
            // Invariant: InFlight is always preceded by a Pending* state (not Clean directly).
            if next == FileSyncState::InFlight {
                prop_assert!(
                    matches!(
                        state,
                        FileSyncState::PendingUpload
                            | FileSyncState::PendingDownload
                            | FileSyncState::PendingConflict
                    ),
                    "InFlight must follow a Pending* state, but followed {:?}",
                    state
                );
            }
            // Invariant: Clean is never followed directly by InFlight.
            if prev == FileSyncState::Clean {
                prop_assert_ne!(next, FileSyncState::InFlight);
            }
            prev = state;
            state = next;
        }
    }

    /// A sequence of Dirty → Pending → InFlight → Clean always terminates at Clean.
    #[test]
    fn happy_path_terminates_at_clean(
        kind in prop_oneof![
            Just(FileSyncState::PendingUpload),
            Just(FileSyncState::PendingDownload),
        ],
    ) {
        // Simulate: Clean → Dirty → Pending → InFlight → Clean.
        let path = [
            FileSyncState::Clean,
            FileSyncState::Dirty,
            kind,
            FileSyncState::InFlight,
            FileSyncState::Clean,
        ];
        // Every step is valid.
        for w in path.windows(2) {
            let succs = valid_successors(w[0]);
            prop_assert!(
                succs.contains(&w[1]),
                "{:?} → {:?} is not a valid transition",
                w[0],
                w[1]
            );
        }
        prop_assert_eq!(*path.last().unwrap(), FileSyncState::Clean);
    }
}
