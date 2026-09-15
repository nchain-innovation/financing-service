//! Telling a backend which addresses to care about.
//!
//! Most backends need no such thing: WhatsOnChain and UaaS index the whole
//! chain, so any address can be queried straight away. A node is different.
//! Balance and UTXO queries reach it through `listunspent`, which reports only
//! what the node's own wallet tracks, so an address the node has never heard
//! of reads as **zero, with no error** — the node is answering truthfully
//! about a wallet that does not know the address.
//!
//! That failure is the unpleasant kind: a reachable node, a successful
//! response, and a balance of nothing. So where a backend needs to be told,
//! the service tells it — at startup for configured clients, and again when a
//! client is added at runtime.

use async_trait::async_trait;
use chain_gang::interface::RpcInterface;

/// A backend that must be told which addresses to watch before it can report
/// anything about them.
///
/// Kept as a trait rather than reaching for `RpcInterface` directly so the
/// service does not depend on which backend is configured, and so the
/// behaviour can be tested without a node.
#[async_trait]
pub trait AddressWatcher: Send + Sync {
    /// Ask the backend to start tracking `address`.
    ///
    /// Implementations should be safe to call for an address already being
    /// watched, since the service calls this on every startup.
    async fn watch_address(&self, address: &str) -> Result<(), String>;
}

#[async_trait]
impl AddressWatcher for RpcInterface {
    async fn watch_address(&self, address: &str) -> Result<(), String> {
        // `false` skips the rescan. A rescan blocks the node's RPC connection
        // while it walks the chain, and the addresses handed to it here are
        // freshly derived client keys with no history to find.
        self.import_address(address, false)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Records what it was asked to watch, and can be told to fail.
///
/// Lets the service's import behaviour be tested without a node.
#[cfg(test)]
pub struct RecordingWatcher {
    pub watched: std::sync::Mutex<Vec<String>>,
    pub fail: bool,
}

#[cfg(test)]
impl RecordingWatcher {
    pub fn new(fail: bool) -> Self {
        Self {
            watched: std::sync::Mutex::new(Vec::new()),
            fail,
        }
    }

    pub fn watched(&self) -> Vec<String> {
        self.watched.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait]
impl AddressWatcher for RecordingWatcher {
    async fn watch_address(&self, address: &str) -> Result<(), String> {
        self.watched.lock().unwrap().push(address.to_string());
        if self.fail {
            return Err("simulated import failure".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_watcher_records_what_it_was_asked_to_watch() {
        let watcher = RecordingWatcher::new(false);
        watcher.watch_address("n1jaAsK").await.unwrap();
        watcher.watch_address("mwxrVFs").await.unwrap();
        assert_eq!(
            watcher.watched(),
            vec!["n1jaAsK".to_string(), "mwxrVFs".to_string()]
        );
    }

    #[tokio::test]
    async fn a_failing_watcher_reports_the_error() {
        let watcher = RecordingWatcher::new(true);
        let error = watcher.watch_address("n1jaAsK").await.unwrap_err();
        assert!(error.contains("simulated import failure"));
        // still recorded, so a caller can see what was attempted
        assert_eq!(watcher.watched().len(), 1);
    }
}
