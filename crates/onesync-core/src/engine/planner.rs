//! Planner: `Decision` list → `Vec<FileOp>`.

use onesync_protocol::{
    enums::{ConflictSide, FileOpKind, FileOpStatus},
    file_op::FileOp,
    id::{PairId, SyncRunId},
    path::RelPath,
    primitives::Timestamp,
};

use crate::engine::types::{Decision, OpPlan};
use crate::limits::MAX_QUEUE_DEPTH_PER_PAIR;
use crate::ports::{Clock, IdGenerator};

/// Convert a list of `(RelPath, Decision)` pairs into an ordered `OpPlan`.
///
/// Sorting rule: shorter paths first (directories before their children).
/// Truncation: stops at `MAX_QUEUE_DEPTH_PER_PAIR` total ops, sets `truncated = true`.
pub fn plan<I: IdGenerator>(
    decisions: Vec<(RelPath, Decision)>,
    pair_id: PairId,
    run_id: SyncRunId,
    clock: &dyn Clock,
    ids: &I,
) -> OpPlan {
    let now = clock.now();
    let mut ops: Vec<FileOp> = Vec::new();
    let mut truncated = false;

    // Sort decisions: parents (shorter paths) before children.
    let mut sorted = decisions;
    sorted.sort_by_key(|(p, _)| p.as_str().len());

    'outer: for (path, decision) in sorted {
        let new_ops = decision_to_ops(&path, decision, pair_id, run_id, ids, now);
        for op in new_ops {
            if ops.len() >= MAX_QUEUE_DEPTH_PER_PAIR {
                truncated = true;
                break 'outer;
            }
            ops.push(op);
        }
    }

    OpPlan { ops, truncated }
}

fn decision_to_ops<I: IdGenerator>(
    path: &RelPath,
    decision: Decision,
    pair_id: PairId,
    run_id: SyncRunId,
    ids: &I,
    now: Timestamp,
) -> Vec<FileOp> {
    let mk = |kind: FileOpKind| FileOp {
        id: ids.new_id(),
        run_id,
        pair_id,
        relative_path: path.clone(),
        kind,
        status: FileOpStatus::Enqueued,
        attempts: 0,
        last_error: None,
        metadata: serde_json::Map::default(),
        enqueued_at: now,
        started_at: None,
        finished_at: None,
    };

    match decision {
        Decision::Clean => Vec::new(),
        Decision::UploadLocalToRemote => vec![mk(FileOpKind::Upload)],
        Decision::DownloadRemoteToLocal => vec![mk(FileOpKind::Download)],
        Decision::DeleteRemote => vec![mk(FileOpKind::RemoteDelete)],
        Decision::DeleteLocal => vec![mk(FileOpKind::LocalDelete)],
        Decision::Conflict {
            winner,
            loser_target,
        } => {
            // 1. Rename the loser on its own side first.
            // 2. Propagate the winner's content to the loser's side (overwrite).
            let rename_op = match winner {
                ConflictSide::Local => mk(FileOpKind::RemoteRename),
                ConflictSide::Remote => mk(FileOpKind::LocalRename),
            };
            let propagate_op = match winner {
                ConflictSide::Local => mk(FileOpKind::Upload),
                ConflictSide::Remote => mk(FileOpKind::Download),
            };
            // Embed the loser target path in rename_op.metadata so the executor knows.
            let mut rename_op = rename_op;
            let mut meta = serde_json::Map::default();
            meta.insert(
                "loser_target".into(),
                serde_json::Value::String(loser_target.as_str().to_owned()),
            );
            rename_op.metadata = meta;
            vec![rename_op, propagate_op]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::ConflictSide;
    use chrono::{TimeZone, Utc};
    use onesync_protocol::{
        id::{Id, PairTag, SyncRunTag},
        path::RelPath,
        primitives::Timestamp,
    };
    use ulid::Ulid;

    struct FakeIds {
        counter: std::sync::Mutex<u64>,
    }
    impl FakeIds {
        fn new() -> Self {
            Self {
                counter: std::sync::Mutex::new(0),
            }
        }
    }
    impl IdGenerator for FakeIds {
        fn new_id<T: onesync_protocol::id::IdPrefix + 'static>(
            &self,
        ) -> onesync_protocol::id::Id<T> {
            let mut guard = self.counter.lock().expect("lock");
            *guard += 1;
            let n = *guard;
            drop(guard);
            // LINT: u64 → u128 widening, no truncation.
            #[allow(clippy::cast_lossless)]
            Id::from_ulid(Ulid::from(n as u128))
        }
    }

    struct FakeClock;
    impl crate::ports::Clock for FakeClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap())
        }
    }

    fn pair_id() -> PairId {
        Id::<PairTag>::from_ulid(Ulid::from(1u128))
    }
    fn run_id() -> SyncRunId {
        Id::<SyncRunTag>::from_ulid(Ulid::from(2u128))
    }
    fn rel(s: &str) -> RelPath {
        s.parse().expect("rel")
    }

    #[test]
    fn clean_decision_produces_no_ops() {
        let decisions = vec![(rel("a.txt"), Decision::Clean)];
        let plan = plan(decisions, pair_id(), run_id(), &FakeClock, &FakeIds::new());
        assert!(plan.ops.is_empty());
        assert!(!plan.truncated);
    }

    #[test]
    fn upload_decision_produces_one_op() {
        let decisions = vec![(rel("a.txt"), Decision::UploadLocalToRemote)];
        let plan = plan(decisions, pair_id(), run_id(), &FakeClock, &FakeIds::new());
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].kind, FileOpKind::Upload);
        assert!(!plan.truncated);
    }

    #[test]
    fn conflict_produces_two_ops_rename_first() {
        let decisions = vec![(
            rel("a.txt"),
            Decision::Conflict {
                winner: ConflictSide::Local,
                loser_target: rel("a (conflict 2026-01-01T00-00-00Z from host).txt"),
            },
        )];
        let plan = plan(decisions, pair_id(), run_id(), &FakeClock, &FakeIds::new());
        assert_eq!(plan.ops.len(), 2);
        // First op renames the remote loser.
        assert_eq!(plan.ops[0].kind, FileOpKind::RemoteRename);
        assert!(plan.ops[0].metadata.contains_key("loser_target"));
        // Second op uploads the local winner.
        assert_eq!(plan.ops[1].kind, FileOpKind::Upload);
    }

    #[test]
    fn truncation_at_max_queue_depth() {
        // Create MAX_QUEUE_DEPTH_PER_PAIR + 1 decisions.
        let decisions: Vec<(RelPath, Decision)> = (0..=MAX_QUEUE_DEPTH_PER_PAIR)
            .map(|i| {
                (
                    format!("file{i}.txt").parse::<RelPath>().expect("rel"),
                    Decision::UploadLocalToRemote,
                )
            })
            .collect();
        let plan = plan(decisions, pair_id(), run_id(), &FakeClock, &FakeIds::new());
        assert_eq!(plan.ops.len(), MAX_QUEUE_DEPTH_PER_PAIR);
        assert!(plan.truncated);
    }
}
