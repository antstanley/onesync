//! Integration test: resync-required handling.
//!
//! The engine catches `GraphError::ResyncRequired` from the first delta call,
//! clears the pair's `delta_token`, transitions to `Initializing`, and re-runs
//! the remote-scan phase as a full initial sync. A second `ResyncRequired`
//! during the same cycle would propagate as a hard error.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod helpers;

use helpers::FakeContext;
use onesync_core::engine::cycle::run_cycle;
use onesync_core::ports::{GraphError, StateStore};
use onesync_protocol::enums::{PairStatus, RunOutcome, RunTrigger};

#[tokio::test]
async fn delta_resync_required_clears_cursor_and_completes() {
    let ctx = FakeContext::new();
    let (pair_id, pair_before) = ctx.seed_pair().await;
    assert!(
        pair_before.delta_token.is_some(),
        "test fixture has a cursor"
    );

    // Inject ResyncRequired on the first delta call; the second call (after
    // the engine clears the cursor) returns the default empty DeltaPage.
    ctx.remote.inject_delta_error(GraphError::ResyncRequired);

    let summary = run_cycle(&ctx.deps(), pair_id, RunTrigger::Scheduled)
        .await
        .expect("resync recovery should complete cleanly");

    assert_eq!(summary.outcome, RunOutcome::Success);

    // The cursor must have been cleared on the persisted pair.
    let pair_after = ctx
        .store
        .pair_get(&pair_id)
        .await
        .expect("get pair")
        .expect("pair present");
    assert!(
        pair_after.delta_token.is_none(),
        "delta_token must be cleared after resync"
    );
    assert_eq!(
        pair_after.status,
        PairStatus::Initializing,
        "pair must be in Initializing during the resync sweep"
    );
}
