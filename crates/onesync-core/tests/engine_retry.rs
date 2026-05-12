//! Integration tests: how the engine surfaces transport-level Graph errors.
//!
//! Neither of these tests exercises the engine's own op-level retry loop (that
//! lives inside the executor and requires populated `DeltaPage` items, which the
//! current port shape doesn't expose). Instead they pin the engine's documented
//! behaviour for delta-time failures:
//!
//! - `Throttled` — bubbles up to the caller as `EngineError::Graph(...)`.
//!   The caller (scheduler / pair worker) is responsible for honouring the
//!   `retry_after_s` and re-scheduling. The engine does not silently retry.
//! - `Unauthorized` — bubbles up the same way; the spec's pair-Errored
//!   transition for delta-time auth failures is wired only inside the op-level
//!   error handling (executor loop), so for now the test pins the current
//!   bubble-up behaviour. The daemon-level retry loop (planned for the
//!   next milestone) will close this gap.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod helpers;

use helpers::FakeContext;
use onesync_core::engine::cycle::run_cycle;
use onesync_core::engine::types::EngineError;
use onesync_core::ports::GraphError;
use onesync_protocol::enums::RunTrigger;

#[tokio::test]
async fn delta_throttled_error_bubbles_up_to_caller() {
    let ctx = FakeContext::new();
    let (pair_id, _pair) = ctx.seed_pair().await;

    ctx.remote
        .inject_delta_error(GraphError::Throttled { retry_after_s: 30 });

    let err = run_cycle(&ctx.deps(), pair_id, RunTrigger::Scheduled)
        .await
        .expect_err("delta throttling should bubble up");

    match err {
        EngineError::Graph(GraphError::Throttled { retry_after_s }) => {
            assert_eq!(retry_after_s, 30);
        }
        other => panic!("expected Throttled, got {other:?}"),
    }
}

#[tokio::test]
async fn delta_unauthorized_error_bubbles_up_to_caller() {
    let ctx = FakeContext::new();
    let (pair_id, _pair) = ctx.seed_pair().await;

    ctx.remote.inject_delta_error(GraphError::Unauthorized);

    let err = run_cycle(&ctx.deps(), pair_id, RunTrigger::Scheduled)
        .await
        .expect_err("delta auth failure should bubble up");

    assert!(
        matches!(err, EngineError::Graph(GraphError::Unauthorized)),
        "expected Unauthorized, got {err:?}"
    );
}
