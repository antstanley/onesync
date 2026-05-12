//! `Jitter` port: supplies a random fraction in `[0, 1)` for backoff computation.
//!
//! Decoupling this from the engine lets tests inject a fixed value for
//! deterministic retry-delay assertions.

/// Supplies a uniform random fraction in `[0.0, 1.0)` used by the retry helper.
pub trait Jitter: Send + Sync {
    /// Return a fresh random fraction in `[0.0, 1.0)`.
    fn next(&self) -> f64;
}
