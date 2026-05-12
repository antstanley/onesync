//! Stable CLI exit codes per `docs/spec/07-cli-and-ipc.md`.

use crate::error::CliError;

/// Map a `CliError` to its documented exit code.
pub const fn exit_code_for(err: &CliError) -> u8 {
    match err {
        CliError::Generic(_) => 1,
        CliError::InvalidArgs(_) => 2,
        CliError::DaemonNotRunning(_) => 3,
        CliError::AuthRequired => 4,
        CliError::PairErrored(_) => 5,
        CliError::ConflictUnresolved(_) => 6,
        CliError::Permission(_) => 7,
        CliError::Network(_) => 8,
        CliError::LimitExceeded(_) => 9,
        CliError::VersionMajorMismatch(_) => 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_maps_to_a_documented_code() {
        // Exit codes 1..=10 are documented.
        let samples = [
            (CliError::Generic("x".into()), 1),
            (CliError::InvalidArgs("x".into()), 2),
            (CliError::DaemonNotRunning("x".into()), 3),
            (CliError::AuthRequired, 4),
            (CliError::PairErrored("x".into()), 5),
            (CliError::ConflictUnresolved("x".into()), 6),
            (CliError::Permission("x".into()), 7),
            (CliError::Network("x".into()), 8),
            (CliError::LimitExceeded("x".into()), 9),
            (CliError::VersionMajorMismatch("x".into()), 10),
        ];
        for (err, expected) in samples {
            assert_eq!(exit_code_for(&err), expected);
        }
    }
}
