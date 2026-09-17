//! Broadcasting through the configured chain-gang `BlockchainInterface`.
//!
//! This is the service's original write path, kept behind the
//! [`TxBroadcaster`] seam so it is interchangeable with mapi-lite. In
//! production the wrapped interface is WhatsOnChain (`interface_type = "woc"`),
//! hence the name; with `uaas`, `rpc` or `test` configured it wraps that
//! interface instead, and reports that name.

use std::sync::Arc;

use async_trait::async_trait;
use chain_gang::{interface::BlockchainInterface, messages::Tx};

use super::{BroadcastError, TxBroadcaster};

/// Hands transactions to the blockchain interface the service also reads from.
pub struct WocBroadcaster {
    inner: Arc<dyn BlockchainInterface + Send + Sync>,
    /// The configured `interface_type`, reported by [`TxBroadcaster::name`].
    name: String,
}

impl WocBroadcaster {
    /// Wrap `inner`, which `interface_type` selected.
    pub fn new(inner: Arc<dyn BlockchainInterface + Send + Sync>, interface_type: &str) -> Self {
        Self {
            inner,
            name: interface_type.to_string(),
        }
    }
}

#[async_trait]
impl TxBroadcaster for WocBroadcaster {
    fn name(&self) -> &str {
        &self.name
    }

    async fn broadcast_tx(&self, tx: &Tx) -> Result<String, BroadcastError> {
        // chain-gang's interfaces do not distinguish a rejection from a
        // transport failure in their error type, so everything is upstream.
        self.inner
            .broadcast_tx(tx)
            .await
            .map_err(|e| BroadcastError::Upstream(e.to_string()))?;
        Ok(tx.hash().encode())
    }

    async fn health_check(&self) -> Result<(), BroadcastError> {
        self.inner
            .status()
            .await
            .map_err(|e| BroadcastError::Upstream(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        sample_tx, test_config, unique_dynamic_config_path, CountingBlockchain,
        FailingBroadcastBlockchain,
    };

    #[tokio::test]
    async fn woc_broadcaster_delegates_to_the_blockchain_interface() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = CountingBlockchain::new(&config).await;
        let broadcaster = WocBroadcaster::new(blockchain.clone(), "test");
        let tx = sample_tx();

        let txid = broadcaster.broadcast_tx(&tx).await.expect("broadcast");

        assert_eq!(txid, tx.hash().encode());
        assert_eq!(blockchain.broadcast_count(), 1);
        assert_eq!(broadcaster.name(), "test");
    }

    #[tokio::test]
    async fn woc_broadcaster_reports_an_interface_failure_as_upstream() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = FailingBroadcastBlockchain::new(&config, 0).await;
        let broadcaster = WocBroadcaster::new(blockchain, "test");

        let error = broadcaster
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("the interface fails every broadcast");

        match error {
            BroadcastError::Upstream(detail) => assert!(detail.contains("simulated")),
            other => panic!("expected an upstream error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn woc_broadcaster_health_check_uses_the_interface_status() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = CountingBlockchain::new(&config).await;
        let broadcaster = WocBroadcaster::new(blockchain, "test");
        assert!(broadcaster.health_check().await.is_ok());
    }
}
