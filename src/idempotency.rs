//! Idempotency records for `POST /fund`.
//!
//! `POST /fund` broadcasts a funding transaction and then returns the outpoint
//! to the client in the response body. If that response is lost -- a client
//! timeout, a dropped connection -- the transaction is already on chain and the
//! client has no record of the outpoint. A plain retry then produces a *second*
//! funding transaction, spending more of the wallet, and the first output is
//! stranded: locked to a key the client may no longer be able to name.
//!
//! A client can avoid that by sending an `idempotency_key` with the request.
//! The first call reserves the key, and once the transaction is broadcast the
//! response is retained against it. A retry carrying the same key replays that
//! stored response instead of funding again.
//!
//! Records live in memory only, so they do not survive a restart. A retry that
//! spans one can still double-fund; that limit is deliberate (it keeps this out
//! of the write path of every funding call) and is documented for callers.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::responses::FundingResponseJson;

/// Identifies a record. Keys are scoped per client so two clients choosing the
/// same key cannot collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordKey {
    pub client_id: String,
    pub key: String,
}

impl RecordKey {
    pub fn new(client_id: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            key: key.into(),
        }
    }
}

/// The outcome of a funding attempt that reached the chain, retained so a
/// retry can be answered with what actually happened rather than being funded
/// again.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Every requested transaction broadcast.
    Funded(FundingResponseJson),
    /// Some transactions broadcast and some did not. The ones that did are in
    /// `response`, and the original failure description is kept so the retry
    /// is answered the same way as the first call.
    PartiallyFunded {
        description: String,
        response: FundingResponseJson,
    },
}

/// What the caller should do with a request that carries an idempotency key.
#[derive(Debug, Clone, PartialEq)]
pub enum Reservation {
    /// The key is new. Proceed with funding; report the outcome via
    /// [`IdempotencyStore::complete`] or [`IdempotencyStore::release`].
    Proceed,
    /// This key already reached the chain. Replay the retained outcome
    /// without preparing or broadcasting anything.
    Replay(Outcome),
    /// A request with this key is still being processed. The caller should not
    /// fund, because doing so risks the duplicate this exists to prevent.
    InProgress,
    /// The key was used before with a materially different request, so
    /// replaying the stored response would answer a question that was not
    /// asked.
    Reused,
}

#[derive(Debug, Clone)]
enum Entry {
    InFlight {
        fingerprint: String,
        at: Instant,
    },
    Completed {
        fingerprint: String,
        outcome: Outcome,
        at: Instant,
    },
}

impl Entry {
    fn at(&self) -> Instant {
        match self {
            Entry::InFlight { at, .. } => *at,
            Entry::Completed { at, .. } => *at,
        }
    }

    fn fingerprint(&self) -> &str {
        match self {
            Entry::InFlight { fingerprint, .. } => fingerprint,
            Entry::Completed { fingerprint, .. } => fingerprint,
        }
    }
}

/// In-memory idempotency records, bounded by both age and count.
#[derive(Debug)]
pub struct IdempotencyStore {
    entries: HashMap<RecordKey, Entry>,
    ttl: Duration,
    max_entries: usize,
}

impl IdempotencyStore {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
            max_entries,
        }
    }

    /// Claim `key` for a request whose identity is `fingerprint`.
    ///
    /// Reserving takes effect immediately, so a concurrent duplicate sees
    /// `InProgress` rather than both callers broadcasting.
    pub fn reserve(&mut self, key: RecordKey, fingerprint: &str) -> Reservation {
        self.evict_expired();

        if let Some(entry) = self.entries.get(&key) {
            if entry.fingerprint() != fingerprint {
                return Reservation::Reused;
            }
            return match entry {
                Entry::InFlight { .. } => Reservation::InProgress,
                Entry::Completed { outcome, .. } => Reservation::Replay(outcome.clone()),
            };
        }

        self.make_room_for_one();
        self.entries.insert(
            key,
            Entry::InFlight {
                fingerprint: fingerprint.to_string(),
                at: Instant::now(),
            },
        );
        Reservation::Proceed
    }

    /// Retain `outcome` against `key`, so a later retry replays it.
    ///
    /// Call this once a funding transaction has been broadcast, whether or not
    /// the local UTXO cache was updated afterwards, and whether the batch
    /// completed or only partly succeeded: the funds have moved, so a retry
    /// must not fund again.
    pub fn complete(&mut self, key: RecordKey, fingerprint: &str, outcome: Outcome) {
        self.entries.insert(
            key,
            Entry::Completed {
                fingerprint: fingerprint.to_string(),
                outcome,
                at: Instant::now(),
            },
        );
    }

    /// Drop the reservation for `key` because nothing was broadcast.
    ///
    /// This lets the client retry the same key, which is the right outcome for
    /// a request that failed before spending anything.
    pub fn release(&mut self, key: &RecordKey) {
        self.entries.remove(key);
    }

    /// Number of retained records. Used by the tests that assert the store
    /// stays bounded.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn evict_expired(&mut self) {
        let ttl = self.ttl;
        let now = Instant::now();
        self.entries
            .retain(|_, entry| now.duration_since(entry.at()) < ttl);
    }

    /// Make space for one new record, dropping the oldest if the store is full.
    fn make_room_for_one(&mut self) {
        while self.entries.len() >= self.max_entries {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.at())
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.entries.remove(&key);
                }
                None => break,
            }
        }
    }
}

/// Identity of a funding request, so a key reused with different parameters is
/// detected rather than silently answered with the earlier response.
pub fn request_fingerprint(
    satoshi: u64,
    no_of_outpoints: u32,
    multiple_tx: bool,
    locking_script: &str,
) -> String {
    format!("{satoshi}:{no_of_outpoints}:{multiple_tx}:{locking_script}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::responses::{OutpointResponse, TxResponse};

    fn response(hash: &str) -> FundingResponseJson {
        FundingResponseJson {
            outpoints: vec![OutpointResponse {
                hash: hash.to_string(),
                index: 1,
            }],
            txs: vec![TxResponse {
                tx: "0100000001".to_string(),
            }],
            replayed: false,
        }
    }

    fn store() -> IdempotencyStore {
        IdempotencyStore::new(Duration::from_secs(600), 10)
    }

    fn key() -> RecordKey {
        RecordKey::new("id1", "abc")
    }

    #[test]
    fn a_new_key_proceeds() {
        let mut store = store();
        assert_eq!(store.reserve(key(), "fp"), Reservation::Proceed);
    }

    #[test]
    fn a_completed_key_replays_the_stored_response() {
        let mut store = store();
        store.reserve(key(), "fp");
        store.complete(key(), "fp", Outcome::Funded(response("aabb")));
        match store.reserve(key(), "fp") {
            Reservation::Replay(Outcome::Funded(replayed)) => {
                assert_eq!(replayed.outpoints[0].hash, "aabb")
            }
            other => panic!("expected Replay, got {other:?}"),
        }
    }

    /// The case the issue is about: the funding transaction is on chain, the
    /// response was lost, and the client retries. It must not fund again.
    #[test]
    fn sr_fund_retry_after_a_lost_response_does_not_fund_again() {
        let mut store = store();
        assert_eq!(store.reserve(key(), "fp"), Reservation::Proceed);
        store.complete(key(), "fp", Outcome::Funded(response("aabb")));
        // the retry
        assert!(matches!(store.reserve(key(), "fp"), Reservation::Replay(_)));
    }

    #[test]
    fn a_concurrent_duplicate_is_in_progress() {
        let mut store = store();
        store.reserve(key(), "fp");
        assert_eq!(store.reserve(key(), "fp"), Reservation::InProgress);
    }

    #[test]
    fn the_same_key_with_a_different_request_is_reused() {
        let mut store = store();
        store.reserve(key(), "fp");
        store.complete(key(), "fp", Outcome::Funded(response("aabb")));
        assert_eq!(store.reserve(key(), "different"), Reservation::Reused);
    }

    #[test]
    fn releasing_lets_the_same_key_be_retried() {
        let mut store = store();
        store.reserve(key(), "fp");
        store.release(&key());
        assert_eq!(store.reserve(key(), "fp"), Reservation::Proceed);
    }

    #[test]
    fn keys_are_scoped_per_client() {
        let mut store = store();
        store.reserve(RecordKey::new("id1", "shared"), "fp");
        store.complete(
            RecordKey::new("id1", "shared"),
            "fp",
            Outcome::Funded(response("aabb")),
        );
        // a different client using the same key string is unaffected
        assert_eq!(
            store.reserve(RecordKey::new("id2", "shared"), "fp"),
            Reservation::Proceed
        );
    }

    #[test]
    fn expired_records_are_forgotten() {
        let mut store = IdempotencyStore::new(Duration::from_nanos(1), 10);
        store.reserve(key(), "fp");
        store.complete(key(), "fp", Outcome::Funded(response("aabb")));
        std::thread::sleep(Duration::from_millis(5));
        // past its TTL, so the key is new again rather than replayable
        assert_eq!(store.reserve(key(), "fp"), Reservation::Proceed);
    }

    #[test]
    fn the_store_is_bounded_by_max_entries() {
        let mut store = IdempotencyStore::new(Duration::from_secs(600), 3);
        for i in 0..10 {
            store.reserve(RecordKey::new("id1", format!("key{i}")), "fp");
        }
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn fingerprint_distinguishes_materially_different_requests() {
        let base = request_fingerprint(123, 1, false, "76a914aa88ac");
        assert_eq!(base, request_fingerprint(123, 1, false, "76a914aa88ac"));
        assert_ne!(base, request_fingerprint(124, 1, false, "76a914aa88ac"));
        assert_ne!(base, request_fingerprint(123, 2, false, "76a914aa88ac"));
        assert_ne!(base, request_fingerprint(123, 1, true, "76a914aa88ac"));
        assert_ne!(base, request_fingerprint(123, 1, false, "76a914bb88ac"));
    }
}
