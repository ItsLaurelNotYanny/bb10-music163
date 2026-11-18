use thiserror::Error;

/// Errors returned when validating client identities, commands, or stop acknowledgements.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SessionError {
    #[error("client_id must not be empty")]
    EmptyClientId,

    #[error("idempotency_key must not be empty")]
    EmptyIdempotencyKey,

    #[error("stale stop acknowledgement: expected pending ({expected_client:?}, gen {expected_gen:?}), got ({got_client}, gen {got_gen})")]
    StaleStopAck {
        expected_client: Option<String>,
        expected_gen: Option<u64>,
        got_client: String,
        got_gen: u64,
    },

    #[error("revoked media generation {ticket_generation} (active generation is {active_generation})")]
    RevokedGeneration {
        ticket_generation: u64,
        active_generation: u64,
    },
}
