//! Production `Jitter` implementation using OS-level randomness.

use onesync_core::ports::Jitter;

/// Samples a uniform fraction from the OS CSPRNG via `getrandom`.
pub struct SystemJitter;

impl Jitter for SystemJitter {
    // LINT: getrandom failing means the OS CSPRNG is unavailable — that's unrecoverable.
    #[allow(clippy::expect_used)]
    fn next(&self) -> f64 {
        let mut buf = [0u8; 8];
        getrandom::getrandom(&mut buf).expect("getrandom failed: OS CSPRNG unavailable");
        let bits = u64::from_le_bytes(buf);
        // Map [0, 2^64) → [0.0, 1.0) by dividing by 2^64.
        #[allow(clippy::cast_precision_loss)]
        let frac = (bits as f64) / (u64::MAX as f64 + 1.0_f64);
        frac.clamp(0.0, 1.0)
    }
}
