//! `AuditSink` port: persistent structured-log destination.

use onesync_protocol::audit::AuditEvent;

/// Sink that consumes structured audit events emitted by the engine.
pub trait AuditSink: Send + Sync {
    /// Emit one event. Implementations are expected to be non-blocking and
    /// to never lose events silently.
    fn emit(&self, event: AuditEvent);
}
