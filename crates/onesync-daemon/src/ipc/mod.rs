//! IPC layer — Unix socket server, line-delimited JSON framing, and dispatch.

// LINT: sub-modules are used by the IPC server task added in Task 11 and
//       wired into async_main in Task 12.
#![allow(dead_code)]

pub mod dispatch;
pub mod framing;
pub mod server;
pub mod subscriptions;
