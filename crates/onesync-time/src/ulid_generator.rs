//! ULID-backed `IdGenerator` adapter.

use std::marker::PhantomData;

use onesync_core::ports::IdGenerator;
use onesync_protocol::id::{Id, IdPrefix};
use ulid::Ulid;

/// `IdGenerator` adapter that produces fresh ULIDs from the system clock + a CSPRNG.
#[derive(Default, Debug)]
pub struct UlidGenerator {
    _marker: PhantomData<()>,
}

impl IdGenerator for UlidGenerator {
    #[allow(clippy::disallowed_methods)]
    // LINT: this is the port-impl; the disallowance is meant for engine code, not for this adapter.
    fn new_id<T: IdPrefix + 'static>(&self) -> Id<T> {
        Id::from_ulid(Ulid::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onesync_protocol::id::AccountTag;

    #[test]
    fn ulid_generator_produces_distinct_ids() {
        let g = UlidGenerator::default();
        let a: Id<AccountTag> = g.new_id();
        let b: Id<AccountTag> = g.new_id();
        assert_ne!(a, b);
    }
}
