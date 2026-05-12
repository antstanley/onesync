//! OAuth 2.0 authentication helpers for Microsoft Graph.
//!
//! Modules:
//! - [`pkce`]: PKCE verifier/challenge generation (RFC 7636)
//! - [`id_token`]: JWT payload decoding for `tid`/`oid`/`upn` claims
//! - [`code_exchange`]: authorization-code grant (`POST /token`)
//! - [`refresh`]: refresh-token grant (`POST /token`)
//! - [`listener`]: loopback HTTP listener for the OAuth redirect

pub mod code_exchange;
pub mod id_token;
pub mod listener;
pub mod pkce;
pub mod refresh;
