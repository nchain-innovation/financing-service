//! Keeping the service inside the blockchain interface's rate limit.
//!
//! WhatsOnChain publishes "up to 3 requests/sec is free" and answers 429 above
//! it. Nothing in the service spaced its calls, and it makes two interface
//! calls per client per refresh -- one for the balance, one for the UTXOs --
//! from a periodic timer, from `GET /balance`, and from `POST /fund` both
//! before building and again on the error path. Several clients, or one client
//! and a little traffic, and the bursts overlap. The reported symptom was a run
//! of 429s within the same second, and the real risk is not the failed refresh:
//! it is being banned for sustained violation, which no amount of retrying
//! recovers from.
//!
//! So the limit is honoured here rather than at each call site. A decorator
//! around [`BlockchainInterface`] sees every outbound *call* whatever asked for
//! it, and no future caller has to remember.
//!
//! # What this paces, and what it does not (CS-457)
//!
//! **One slot per interface call, not per HTTP request.** Those were the same
//! thing when this was written against chain-gang 0.11.2. They stopped being
//! the same in 0.11.3 (CS-456): `get_balance` is now two requests, and
//! `get_utxo` is one per 1000 UTXOs -- 21 for the mainnet address CS-456
//! measured. The limiter reserves one slot and chain-gang then issues all of
//! them inside it.
//!
//! This cannot be fixed from here. `WocInterface` builds its own client and
//! calls `reqwest::get` directly, and `WocInterface::new` takes no arguments,
//! so there is no client, middleware or hook to supply -- the paging requests
//! are unreachable from this crate by construction. Restoring the guarantee
//! needs an upstream change; CS-457 carries the analysis and the options.
//!
//! So read `max_requests_per_second` as spacing between *calls*. For an address
//! under 1000 UTXOs that is still one request each and the original guarantee
//! holds exactly; beyond it, the configured number is a floor on call spacing
//! rather than a ceiling on request rate.
//!
//! Only the interfaces that talk to someone else's server need this. A node
//! reached over RPC, or a local UaaS, is the operator's own and is left
//! unthrottled by default.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chain_gang::interface::{Balance, BlockchainInterface, Utxo};
use chain_gang::messages::{BlockHeader, Tx};
use chain_gang::network::Network;
use chain_gang::util::ChainGangError;
use tokio::sync::Mutex;

/// Wraps a blockchain interface and spaces its outbound requests.
pub struct RateLimited {
    inner: Arc<dyn BlockchainInterface + Send + Sync>,
    /// Minimum gap between the starts of two requests.
    min_interval: Duration,
    /// The earliest a request may start. Each caller reserves a slot and moves
    /// this on, so concurrent callers queue rather than collide.
    next_slot: Mutex<Instant>,
}

impl RateLimited {
    /// Wrap `inner` at `requests_per_second`.
    pub fn new(
        inner: Arc<dyn BlockchainInterface + Send + Sync>,
        requests_per_second: u32,
    ) -> Self {
        let rps = requests_per_second.max(1);
        Self {
            inner,
            min_interval: Duration::from_secs(1) / rps,
            next_slot: Mutex::new(Instant::now()),
        }
    }

    /// Wait until this request's turn.
    ///
    /// The slot is reserved under the lock, and the lock released before the
    /// sleep. That is not a throughput trick -- holding it across the sleep
    /// finishes every caller at the same moment, since each wakes exactly at
    /// the slot it reserved -- it is to avoid holding a lock across an await
    /// at all, so nothing that later takes this lock for a cheap read can be
    /// parked behind somebody else's wait.
    async fn acquire(&self) {
        let wait = {
            let mut next_slot = self.next_slot.lock().await;
            let now = Instant::now();
            let start = (*next_slot).max(now);
            *next_slot = start + self.min_interval;
            start.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[async_trait]
impl BlockchainInterface for RateLimited {
    fn set_network(&mut self, _network: &Network) {
        // The wrapped interface is configured before it is wrapped. Taking
        // `&mut self` here cannot reach through an `Arc` anyway, and silently
        // doing nothing is better than pretending: the factory sets the
        // network on the inner interface first.
    }

    async fn status(&self) -> Result<(), ChainGangError> {
        self.acquire().await;
        self.inner.status().await
    }

    async fn get_balance(&self, address: &str) -> Result<Balance, ChainGangError> {
        self.acquire().await;
        self.inner.get_balance(address).await
    }

    async fn get_utxo(&self, address: &str) -> Result<Utxo, ChainGangError> {
        self.acquire().await;
        self.inner.get_utxo(address).await
    }

    async fn broadcast_tx(&self, tx: &Tx) -> Result<String, ChainGangError> {
        self.acquire().await;
        self.inner.broadcast_tx(tx).await
    }

    async fn get_tx(&self, txid: &str) -> Result<Tx, ChainGangError> {
        self.acquire().await;
        self.inner.get_tx(txid).await
    }

    async fn get_latest_block_header(&self) -> Result<BlockHeader, ChainGangError> {
        self.acquire().await;
        self.inner.get_latest_block_header().await
    }

    async fn get_block_headers(&self) -> Result<String, ChainGangError> {
        self.acquire().await;
        self.inner.get_block_headers().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_blockchain_interface, test_config, TEST_ADDRESS};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Counts calls and answers instantly, so what a test measures is the
    /// limiter's spacing and nothing else.
    struct CountingInterface {
        inner: Arc<dyn BlockchainInterface + Send + Sync>,
        calls: AtomicU32,
    }

    #[async_trait]
    impl BlockchainInterface for CountingInterface {
        fn set_network(&mut self, _network: &Network) {}
        async fn status(&self) -> Result<(), ChainGangError> {
            Ok(())
        }
        async fn get_balance(&self, address: &str) -> Result<Balance, ChainGangError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.get_balance(address).await
        }
        async fn get_utxo(&self, address: &str) -> Result<Utxo, ChainGangError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.get_utxo(address).await
        }
        async fn broadcast_tx(&self, _tx: &Tx) -> Result<String, ChainGangError> {
            unimplemented!()
        }
        async fn get_tx(&self, _txid: &str) -> Result<Tx, ChainGangError> {
            unimplemented!()
        }
        async fn get_latest_block_header(&self) -> Result<BlockHeader, ChainGangError> {
            unimplemented!()
        }
        async fn get_block_headers(&self) -> Result<String, ChainGangError> {
            unimplemented!()
        }
    }

    async fn counting() -> Arc<CountingInterface> {
        let config = test_config("/tmp/financing-service-rate-limit-test.toml");
        Arc::new(CountingInterface {
            inner: test_blockchain_interface(&config).await,
            calls: AtomicU32::new(0),
        })
    }

    /// The point of the whole module: a burst of calls is spread out instead
    /// of leaving all at once. Six requests at 3/s cannot finish before the
    /// gaps between them have been waited out.
    #[tokio::test]
    async fn cs_418_a_burst_is_spaced_out_rather_than_sent_at_once() {
        let counter = counting().await;
        let limited = RateLimited::new(counter.clone(), 3);

        let started = Instant::now();
        for _ in 0..6 {
            limited.get_balance(TEST_ADDRESS).await.expect("balance");
        }
        let elapsed = started.elapsed();

        assert_eq!(counter.calls.load(Ordering::SeqCst), 6);
        // Five gaps of a third of a second between six calls.
        assert!(
            elapsed >= Duration::from_millis(1600),
            "six calls at 3/s took {elapsed:?}, which is faster than the limit allows"
        );
    }

    /// Concurrent callers have to share the same allowance -- a limiter that
    /// only spaced one caller's calls would be no limiter at all, since the
    /// service refreshes several clients at once.
    #[tokio::test]
    async fn cs_418_concurrent_callers_share_the_allowance() {
        let counter = counting().await;
        let limited = Arc::new(RateLimited::new(counter.clone(), 3));

        let started = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..6 {
            let limited = Arc::clone(&limited);
            handles.push(tokio::spawn(async move {
                limited.get_utxo(TEST_ADDRESS).await.expect("utxo")
            }));
        }
        for handle in handles {
            handle.await.expect("joined");
        }
        let elapsed = started.elapsed();

        assert_eq!(counter.calls.load(Ordering::SeqCst), 6);
        assert!(
            elapsed >= Duration::from_millis(1600),
            "six concurrent calls at 3/s took {elapsed:?}: they did not share the allowance"
        );
    }
}
