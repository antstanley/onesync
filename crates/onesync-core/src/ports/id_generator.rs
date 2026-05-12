//! `IdGenerator` port: source of typed identifiers.

use onesync_protocol::id::{Id, IdPrefix};

/// Source of fresh typed identifiers.
pub trait IdGenerator: Send + Sync {
    /// Produces a new identifier with prefix `T::PREFIX`.
    fn new_id<T: IdPrefix + 'static>(&self) -> Id<T>;
}
