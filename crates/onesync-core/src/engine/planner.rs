//! Planner: converts a list of [`Decision`]s into an ordered [`Vec<FileOp>`].
//!
//! Ordering rules (applied in sequence):
//! 1. `LocalMkdir` / `RemoteMkdir` before any file operations in that directory.
//! 2. Deletions after moves/creates that replace the deleted path.
//! 3. All other operations are appended in input order.
//!
//! The planner is synchronous and pure: it does no I/O.

use onesync_protocol::{
    enums::{FileOpKind, FileOpStatus},
    file_op::FileOp,
    id::{FileOpId, PairId, SyncRunId},
    path::RelPath,
    primitives::Timestamp,
};

use crate::{engine::types::Decision, ports::IdGenerator};

/// Expand `decisions` into an ordered sequence of [`FileOp`]s.
///
/// `run_id` and `now` are stamped onto every op. `ids` supplies fresh
/// [`FileOpId`] values (one per op).
///
/// `I` must implement [`IdGenerator`]; a generic parameter is required because
/// [`IdGenerator`] is not dyn-compatible (its `new_id` method is generic).
#[must_use]
pub fn plan<I: IdGenerator>(
    decisions: Vec<Decision>,
    run_id: SyncRunId,
    now: Timestamp,
    ids: &I,
) -> Vec<FileOp> {
    let mut mkdir_ops: Vec<FileOp> = Vec::new();
    let mut delete_ops: Vec<FileOp> = Vec::new();
    let mut other_ops: Vec<FileOp> = Vec::new();

    for decision in decisions {
        let Some(kind) = decision.kind.to_file_op_kind() else {
            // NoOp and Conflict (caller resolves conflict separately).
            continue;
        };

        let op = make_op(
            ids.new_id(),
            run_id,
            decision.pair_id,
            decision.relative_path,
            kind,
            now,
        );

        match kind {
            FileOpKind::LocalMkdir | FileOpKind::RemoteMkdir => mkdir_ops.push(op),
            FileOpKind::LocalDelete | FileOpKind::RemoteDelete => delete_ops.push(op),
            _ => other_ops.push(op),
        }
    }

    // mkdir first, then creates/uploads/downloads/renames, then deletes.
    mkdir_ops.extend(other_ops);
    mkdir_ops.extend(delete_ops);
    mkdir_ops
}

fn make_op(
    id: FileOpId,
    run_id: SyncRunId,
    pair_id: PairId,
    relative_path: RelPath,
    kind: FileOpKind,
    now: Timestamp,
) -> FileOp {
    FileOp {
        id,
        run_id,
        pair_id,
        relative_path,
        kind,
        status: FileOpStatus::Enqueued,
        attempts: 0,
        last_error: None,
        metadata: serde_json::Map::new(),
        enqueued_at: now,
        started_at: None,
        finished_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use onesync_protocol::{
        enums::FileOpKind,
        id::{Id, IdPrefix, PairId, SyncRunId},
        primitives::Timestamp,
    };
    use ulid::Ulid;

    use crate::engine::types::{Decision, DecisionKind};

    struct SeqIds(std::sync::atomic::AtomicU64);

    impl SeqIds {
        fn new() -> Self {
            Self(std::sync::atomic::AtomicU64::new(1))
        }
    }

    impl IdGenerator for SeqIds {
        fn new_id<T: IdPrefix + 'static>(&self) -> Id<T> {
            let n = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Construct a deterministic ULID from the counter for test purposes.
            // LINT: wrapping is fine — test only.
            #[allow(clippy::disallowed_methods)]
            let ulid = Ulid::from_parts(n, 0);
            Id::from_ulid(ulid)
        }
    }

    fn pair() -> PairId {
        // LINT: Ulid::new() allowed in tests.
        #[allow(clippy::disallowed_methods)]
        PairId::from_ulid(Ulid::new())
    }

    fn run() -> SyncRunId {
        // LINT: Ulid::new() allowed in tests.
        #[allow(clippy::disallowed_methods)]
        SyncRunId::from_ulid(Ulid::new())
    }

    fn now() -> Timestamp {
        // LINT: Utc::now() allowed in tests.
        #[allow(clippy::disallowed_methods)]
        Timestamp::from_datetime(Utc.timestamp_opt(1_700_000_000, 0).unwrap())
    }

    fn decision(p: PairId, path: &str, kind: DecisionKind) -> Decision {
        Decision {
            pair_id: p,
            relative_path: path.parse().unwrap(),
            kind,
        }
    }

    #[test]
    fn noop_decisions_produce_no_ops() {
        let p = pair();
        let decisions = vec![decision(p, "a.txt", DecisionKind::NoOp)];
        let ops = plan(decisions, run(), now(), &SeqIds::new());
        assert!(ops.is_empty());
    }

    #[test]
    fn mkdir_ops_precede_file_ops() {
        let p = pair();
        let decisions = vec![
            decision(p, "docs/file.txt", DecisionKind::Download),
            decision(p, "docs", DecisionKind::LocalMkdir),
        ];
        let ops = plan(decisions, run(), now(), &SeqIds::new());
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].kind, FileOpKind::LocalMkdir);
        assert_eq!(ops[1].kind, FileOpKind::Download);
    }

    #[test]
    fn delete_ops_come_last() {
        let p = pair();
        let decisions = vec![
            decision(p, "old.txt", DecisionKind::LocalDelete),
            decision(p, "new.txt", DecisionKind::Upload),
        ];
        let ops = plan(decisions, run(), now(), &SeqIds::new());
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].kind, FileOpKind::Upload);
        assert_eq!(ops[1].kind, FileOpKind::LocalDelete);
    }

    #[test]
    fn ops_have_enqueued_status_and_zero_attempts() {
        let p = pair();
        let decisions = vec![decision(p, "a.txt", DecisionKind::Upload)];
        let ops = plan(decisions, run(), now(), &SeqIds::new());
        assert_eq!(ops[0].status, FileOpStatus::Enqueued);
        assert_eq!(ops[0].attempts, 0);
    }
}
