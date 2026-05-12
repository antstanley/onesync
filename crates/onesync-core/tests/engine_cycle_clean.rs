//! Integration test: a no-op cycle on a fully Clean pair.
//!
//! Pre-seed: pair with no file entries, empty fake fs and remote drive.
//! Expected: `CycleSummary { local_ops: 0, remote_ops: 0, outcome: Success }`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod helpers;

use helpers::FakeContext;
use onesync_core::engine::cycle::run_cycle;
use onesync_protocol::enums::{RunOutcome, RunTrigger};

#[tokio::test]
async fn clean_cycle_is_a_no_op() {
    let ctx = FakeContext::new();
    let (pair_id, _pair) = ctx.seed_pair().await;

    let summary = run_cycle(&ctx.deps(), pair_id, RunTrigger::Scheduled)
        .await
        .expect("cycle should succeed");

    assert_eq!(summary.local_ops, 0);
    assert_eq!(summary.remote_ops, 0);
    assert_eq!(summary.outcome, RunOutcome::Success);
    assert_eq!(summary.pair_id, pair_id);
}
