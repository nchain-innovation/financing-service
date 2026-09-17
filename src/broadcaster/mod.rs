//! Sending signed funding transactions to the network.
//!
//! The service reads chain state (balances, UTXOs) through chain-gang's
//! `BlockchainInterface` and, until now, broadcast through the same object.
//! The two are separate concerns with separate failure modes -- a public
//! explorer that answers reads fine may still be the wrong place to hand a
//! transaction -- so the write path has its own seam here.
//!
//! Two implementations exist. [`woc::WocBroadcaster`] wraps whatever
//! `BlockchainInterface` the `[blockchain_interface]` section configured (WoC
//! in production), which is exactly the behaviour the service had before this
//! module existed. [`mapi::MapiBroadcaster`] hands transactions to a mapi-lite
//! server instead, and is selected by the presence of a `[mapi_lite]` section
//! -- see [`factory::broadcaster_factory`]. Reads stay on the blockchain
//! interface either way.

pub mod factory;
pub mod mapi;
pub mod woc;

use async_trait::async_trait;
use chain_gang::messages::Tx;

/// [`TxBroadcaster::name`] of the mapi-lite implementation. The service uses
/// it to decide whether `GET /ready` has an upstream to probe.
pub const MAPI_LITE: &str = "mapi-lite";

/// Why a broadcast did not succeed.
///
/// The split matters to a caller deciding what to do next, and to this service
/// deciding what to believe about its own wallet. A rejection is the upstream's
/// verdict on the transaction; an upstream error says nothing about the
/// transaction at all; an indeterminate outcome says the transaction may be on
/// the network and we cannot find out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastError {
    /// The upstream answered and refused the transaction.
    Rejected {
        description: String,
        /// The upstream's own view of whether resubmitting could succeed.
        retryable: bool,
    },
    /// The upstream could not be reached or did not answer usably: transport
    /// failure, HTTP error status, undecodable body, bad response signature.
    Upstream(String),
    /// The transaction was handed to the upstream and its fate is unknown.
    ///
    /// Distinct from [`BroadcastError::Upstream`] because it is not a failure,
    /// it is an absence of information, and the two call for opposite
    /// handling. A transaction that never left cannot have spent anything; a
    /// transaction that may have left may have spent its inputs, and the
    /// service must not offer those inputs to the next funding request.
    Indeterminate(String),
}

impl std::fmt::Display for BroadcastError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BroadcastError::Rejected {
                description,
                retryable,
            } => write!(f, "rejected: {description} (retryable: {retryable})"),
            BroadcastError::Upstream(detail) => write!(f, "upstream error: {detail}"),
            BroadcastError::Indeterminate(detail) => {
                write!(f, "outcome unknown: {detail}")
            }
        }
    }
}

impl std::error::Error for BroadcastError {}

/// Where funding transactions are sent.
///
/// One implementation is chosen at startup from configuration and shared by
/// every funding request. Implementations must be safe to call concurrently:
/// the service does not serialise broadcasts.
#[async_trait]
pub trait TxBroadcaster: Send + Sync {
    /// Short name for logs and `GET /status`: the `interface_type` for the
    /// blockchain-interface broadcaster, [`MAPI_LITE`] for mapi-lite.
    fn name(&self) -> &str;

    /// Send `tx` to the network. Returns the txid (hex) as the upstream
    /// reported it, which for a well-behaved upstream equals `tx.hash()`.
    async fn broadcast_tx(&self, tx: &Tx) -> Result<String, BroadcastError>;

    /// Reachability probe, used at startup and by `GET /ready`. Should be
    /// cheap and bounded in time: a readiness probe allows a few seconds in
    /// total.
    async fn health_check(&self) -> Result<(), BroadcastError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The response body carries a fixed description, so this Display is the
    /// only place an operator learns *why* the upstream refused a transaction
    /// and whether it thought a retry could help. `Service` logs the error
    /// through it on every broadcast failure.
    #[test]
    fn broadcast_error_display_names_the_kind_of_failure() {
        let rejected = BroadcastError::Rejected {
            description: "txn-mempool-conflict".to_string(),
            retryable: false,
        };
        assert_eq!(
            rejected.to_string(),
            "rejected: txn-mempool-conflict (retryable: false)"
        );
        let retryable = BroadcastError::Rejected {
            description: "mempool full".to_string(),
            retryable: true,
        };
        assert_eq!(
            retryable.to_string(),
            "rejected: mempool full (retryable: true)"
        );
        let upstream = BroadcastError::Upstream("http status 503".to_string());
        assert_eq!(upstream.to_string(), "upstream error: http status 503");
        let unknown = BroadcastError::Indeterminate("no answer in 45s".to_string());
        assert_eq!(unknown.to_string(), "outcome unknown: no answer in 45s");
    }
}
