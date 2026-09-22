//! Broadcasting through a mapi-lite server, using the `uls-client` crate.
//!
//! mapi-lite is nChain's Rust MerchantAPI surface; `uls-client` is its typed
//! HTTP client and shares the wire types with the server, so this module never
//! spells out JSON. A submit is `POST /mapi/txs` with a one-element batch; the
//! health probe is `GET /mapi/feeQuote`, which `uls-client` documents as the
//! readiness probe -- it is authenticated and signed, so a pass proves the
//! token and the response-verification path both work, which is exactly what
//! a submit needs.
//!
//! Two clients share one base URL and token: the submit client and the probe
//! client, the latter with a short timeout and no retries, because `GET
//! /ready` has to answer inside a readiness probe's few seconds however slow
//! mapi-lite is being.
//!
//! **Retries are this module's, not uls-client's.** uls-client will retry a
//! submit for us, but it reports the whole sequence as one `RetriesExhausted`
//! whose cause is a string, and a refused connection and an expired timeout
//! render identically there. The service needs that distinction: a refused
//! connection delivered nothing, while an attempt cancelled in flight may
//! have delivered everything, and only the second calls for the funding
//! inputs to be reserved (see `BroadcastError::Indeterminate` and
//! `Client::commit_uncertain_funding_spend`). Running the loop here keeps
//! every attempt's outcome structural.
//!
//! Each attempt is bounded by `mapi_lite.timeout_seconds` and the whole
//! sequence by `mapi_lite.total_timeout_seconds`. A funding call is
//! interactive: a wedged mapi-lite must not hold the caller, and the actix
//! worker serving it, for `timeout * (max_retries + 1)`.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chain_gang::messages::Tx;
use uls_client::{ClientError, MapiClient, SubmitTxRequest};
use uls_core::status::RESULT_SUCCESS;
use uls_core::wire::FeeAmount;

/// The fee quote entry that applies to ordinary transactions. mapi quotes a
/// "standard" and a "data" rate; funding transactions are the former.
const STANDARD_FEE_TYPE: &str = "standard";

use super::{BroadcastError, TxBroadcaster, MAPI_LITE};
use crate::{config::MapiLiteConfig, util::tx_as_hexstr};

/// Wait between submit retries: linear back-off, `attempt * BASE` capped at
/// `CAP`. uls-client's own default caps at 60s, which would hold a `/fund`
/// request open for minutes; a funding call is interactive.
const RETRY_BACKOFF_BASE: Duration = Duration::from_secs(1);
const RETRY_BACKOFF_CAP: Duration = Duration::from_secs(5);

/// Hands transactions to a mapi-lite server.
pub struct MapiBroadcaster {
    /// Built with uls-client's own retries left off: the loop in
    /// [`MapiBroadcaster::broadcast_tx`] owns them.
    submit: MapiClient,
    probe: MapiClient,
    /// Ceiling on one attempt.
    attempt_timeout: Duration,
    /// Attempts after the first.
    max_retries: u32,
    /// Ceiling on a whole submit, retries and back-off included, so a `/fund`
    /// caller cannot be held for `timeout * (max_retries + 1)`.
    submit_deadline: Duration,
}

impl MapiBroadcaster {
    /// Build a broadcaster for the server named by `config`.
    pub fn new(config: &MapiLiteConfig) -> Result<Self, String> {
        if let Some(warning) = config.retry_budget_warning() {
            log::warn!("{warning}");
        }
        // uls-client retries five times by default with a 60s back-off cap,
        // so the retries have to be turned *off* explicitly -- left alone they
        // would nest inside every attempt this module makes and blow through
        // both bounds. Zero means one HTTP request per call, which is what the
        // loop below is counting.
        //
        // The reqwest timeout is only a backstop against a connection the
        // per-attempt bound somehow outlives; that bound is never larger than
        // the deadline, so it always fires first.
        let submit = Self::client(config, config.total_timeout() + Duration::from_secs(5))?.retry(
            0,
            RETRY_BACKOFF_BASE,
            RETRY_BACKOFF_CAP,
        );
        // fee_quote does not retry, so the probe needs no retry settings.
        let probe = Self::client(config, config.health_timeout())?;
        Ok(Self {
            submit,
            probe,
            attempt_timeout: config.timeout(),
            max_retries: config.max_retries,
            submit_deadline: config.total_timeout(),
        })
    }

    /// Wait before attempt `attempt + 1`, matching uls-client's linear
    /// back-off so moving the loop here does not change the pacing.
    fn backoff(attempt: u32) -> Duration {
        RETRY_BACKOFF_BASE
            .saturating_mul(attempt)
            .min(RETRY_BACKOFF_CAP)
    }

    /// Turn a submit response into a txid or an error.
    ///
    /// Everything here is the server's answer about our transaction, so none
    /// of it is retried: it is settled, however unwelcome.
    fn read_result(
        payload: uls_client::TxsPayload,
        expected: &str,
    ) -> Result<String, BroadcastError> {
        // One in, one out. Anything else means the server and this client
        // disagree about the batch contract, and guessing which result is ours
        // would be worse than refusing.
        let [result] = payload.txs.as_slice() else {
            // The server answered, so it took the transaction, but the answer
            // is not one this client can read. What it did with the
            // transaction is therefore unknown rather than known to have
            // failed.
            return Err(BroadcastError::Indeterminate(format!(
                "submitted 1 transaction but mapi-lite returned {} results",
                payload.txs.len()
            )));
        };

        if result.return_result != RESULT_SUCCESS {
            let conflicts: Vec<&str> = result
                .conflicted_with
                .iter()
                .map(|conflict| conflict.txid.as_str())
                .collect();
            let description = if conflicts.is_empty() {
                result.result_description.clone()
            } else {
                format!(
                    "{} (conflicted with {})",
                    result.result_description,
                    conflicts.join(", ")
                )
            };
            // A conflict is final whatever the server says about retrying.
            //
            // `txn-mempool-conflict` means another transaction already holds
            // an input of this one. Offering the identical transaction again
            // cannot succeed -- the conflict is in the inputs, and they are
            // gone. mapi-lite reports it as `failureRetryable: true`, which is
            // how a caller ended up being told "retry may succeed" about a
            // double spend that never would.
            //
            // Trusting the flag for everything else: this narrows one case the
            // response itself contradicts, rather than second-guessing the
            // upstream's judgement in general.
            let retryable = result.failure_retryable && conflicts.is_empty();
            return Err(BroadcastError::Rejected {
                description,
                retryable,
            });
        }

        // "Already known" also comes back as success: a resubmitted funding
        // transaction is the same transaction, so that is the right answer --
        // and it is how a retry after an unknown outcome resolves into a
        // known one.
        //
        // The txid must be the one we hashed. A different one means what
        // reached the network is not the transaction this service built, and
        // the caller would otherwise be handed outpoints -- derived from the
        // local hash -- for a transaction that will never confirm, with its
        // UTXOs already marked spent. Refused for the same reason the batch
        // length is: a disagreement here is not ours to guess through.
        //
        // Unknown rather than failed, though: the server reported success, so
        // something was accepted, and this service cannot tell whether the
        // transaction it built was part of it.
        if result.txid != expected {
            return Err(BroadcastError::Indeterminate(format!(
                "mapi-lite reported txid {} for a funding transaction hashing to {expected}",
                result.txid
            )));
        }
        Ok(result.txid.clone())
    }

    /// The error for a submit that ran out of deadline between attempts.
    ///
    /// Whether that is a failure or an unknown outcome turns on what the
    /// attempts did, not on the clock: a mapi-lite that refuses connections
    /// burns the deadline in back-off alone and delivered nothing, while one
    /// that goes quiet burns it inside a request that may have been received.
    fn out_of_time(&self, attempts: u32, may_have_landed: bool) -> BroadcastError {
        let detail = format!(
            "mapi-lite did not answer within {}s (mapi_lite.total_timeout_seconds) over {attempts} \
             attempt(s)",
            self.submit_deadline.as_secs()
        );
        if may_have_landed {
            BroadcastError::Indeterminate(detail)
        } else {
            BroadcastError::Upstream(detail)
        }
    }

    /// The error a finished submit reports, given what its attempts might have
    /// delivered.
    ///
    /// A plain upstream failure on the last attempt does not make the call a
    /// plain failure: if an earlier attempt was cancelled in flight, the
    /// transaction may be on the network and the inputs must still be
    /// reserved.
    fn settle(&self, last: BroadcastError, may_have_landed: bool) -> BroadcastError {
        match last {
            BroadcastError::Upstream(detail) if may_have_landed => BroadcastError::Indeterminate(
                format!("{detail}; an earlier attempt may have been delivered"),
            ),
            other => other,
        }
    }

    /// A `MapiClient` for `config` whose every request times out after
    /// `timeout`. uls-client has no timeout setting of its own; the injected
    /// reqwest client is the knob.
    fn client(config: &MapiLiteConfig, timeout: Duration) -> Result<MapiClient, String> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| format!("Unable to build the mapi-lite HTTP client: {e}"))?;
        let mut client = MapiClient::new(config.base_url().to_string()).with_http(http);
        if let Some(token) = &config.auth_token {
            // Sent verbatim as the Authorization header; the scheme is the
            // operator's to include (see MapiLiteConfig::auth_token).
            client = client.with_token(token.clone());
        }
        Ok(client)
    }
}

impl From<ClientError> for BroadcastError {
    /// Split a client error by what it says about the transaction, not by how
    /// bad it looks.
    ///
    /// The question is only ever "might mapi-lite have taken this and relayed
    /// it?". A connection that was refused took nothing. A request that was
    /// sent and never answered may have taken everything.
    fn from(error: ClientError) -> Self {
        let detail = error.to_string();
        match error {
            // Sent, and no answer came back within the request timeout. The
            // server may have relayed the transaction and been slow to say so.
            ClientError::Transport(e) if e.is_timeout() => BroadcastError::Indeterminate(detail),
            // The server answered and the answer is unreadable, so it took the
            // transaction and its verdict is lost to us.
            ClientError::Decode(_) | ClientError::BadSignature => {
                BroadcastError::Indeterminate(detail)
            }
            // A refused or unresolvable connection never delivered anything,
            // and an HTTP error status is the server declining to act. Both
            // leave the transaction where it was built.
            //
            // `RetriesExhausted` lands here too, and that one is a judgement
            // call: uls-client renders the last attempt's cause to a string,
            // and a refused connection and an expired timeout render
            // identically, so the distinction cannot be recovered. Calling it
            // determinate is right for the failure that actually produces it
            // in practice -- a mapi-lite that is down refuses connections in
            // milliseconds and exhausts the budget long before the submit
            // deadline, while a mapi-lite that is merely slow burns the
            // deadline first and is reported as indeterminate by the caller
            // above. That argument holds only while the retry budget cannot
            // fit inside `total_timeout_seconds`; see the note in
            // `MapiBroadcaster::broadcast_tx`.
            _ => BroadcastError::Upstream(detail),
        }
    }
}

#[async_trait]
impl TxBroadcaster for MapiBroadcaster {
    fn name(&self) -> &str {
        MAPI_LITE
    }

    async fn broadcast_tx(&self, tx: &Tx) -> Result<String, BroadcastError> {
        let request = SubmitTxRequest {
            raw_tx: tx_as_hexstr(tx).map_err(BroadcastError::Upstream)?,
            callback_url: None,
            callback_token: None,
            // The server defaults this to true when omitted. A funding
            // transaction's proof is of no use to this service, and asking for
            // one only enlarges the response.
            merkle_proof: Some(false),
            merkle_format: None,
        };
        let expected = tx.hash().encode();

        let started = Instant::now();
        let mut attempt: u32 = 0;
        // Set the moment an attempt might have reached the server: a request
        // cancelled in flight, or an answer this client could not read. It is
        // never cleared, because a later refused connection cannot un-deliver
        // an earlier request. Only a definite answer about the transaction --
        // accepted, or rejected -- settles it.
        let mut may_have_landed = false;

        loop {
            attempt += 1;
            let remaining = self.submit_deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(self.out_of_time(attempt - 1, may_have_landed));
            }
            // Whichever runs out first. An attempt is never allowed to outlive
            // the deadline it sits inside.
            let bound = self.attempt_timeout.min(remaining);

            let error = match tokio::time::timeout(
                bound,
                self.submit
                    .submit_transactions(std::slice::from_ref(&request)),
            )
            .await
            {
                Ok(Ok(payload)) => return Self::read_result(payload, &expected),
                Ok(Err(client_error)) => BroadcastError::from(client_error),
                Err(_) => {
                    // Cancelling the request says nothing about what the
                    // server did with it: a mapi-lite that is slow rather than
                    // broken may have relayed the transaction already and be
                    // about to answer.
                    BroadcastError::Indeterminate(format!(
                        "mapi-lite did not answer attempt {attempt} within {}s \
                         (mapi_lite.timeout_seconds)",
                        bound.as_secs()
                    ))
                }
            };

            if matches!(error, BroadcastError::Indeterminate(_)) {
                may_have_landed = true;
            }

            if attempt > self.max_retries {
                return Err(self.settle(error, may_have_landed));
            }

            // Retrying is safe, and after an unknown outcome it is actively
            // useful: mapi-lite answers a transaction it already holds with
            // success, so a retry can turn "we do not know" into "it is on the
            // network" -- which is the difference between reserving the
            // client's inputs and committing them.
            let backoff = Self::backoff(attempt);
            let remaining = self.submit_deadline.saturating_sub(started.elapsed());
            if backoff >= remaining {
                return Err(self.out_of_time(attempt, may_have_landed));
            }
            tokio::time::sleep(backoff).await;
        }
    }

    async fn health_check(&self) -> Result<(), BroadcastError> {
        self.probe.fee_quote().await?;
        Ok(())
    }

    /// The standard mining fee from mapi-lite's quote, converted to satoshis
    /// per kilobyte (CS-451).
    ///
    /// The quote states a rate as a pair -- so many satoshis per so many bytes
    /// -- rather than per kilobyte, so it is scaled here, rounding up so the
    /// service never charges itself less than the miner asked for.
    ///
    /// `None` on any failure, which leaves the configured rate standing: a
    /// quote that cannot be fetched, is unsigned, names no standard fee, or
    /// gives a nonsensical zero rate is not a reason to stop funding. The
    /// probe client is used because, like the health check, this wants a short
    /// timeout and no retries.
    async fn fee_satoshis_per_kb(&self) -> Option<u64> {
        let quote = match self.probe.fee_quote().await {
            Ok(quote) => quote,
            Err(e) => {
                log::warn!("fee quote failed, keeping the current fee rate: {e}");
                return None;
            }
        };
        let standard = quote
            .fees
            .iter()
            .find(|fee| fee.fee_type.eq_ignore_ascii_case(STANDARD_FEE_TYPE))
            // A quote that names only one fee is taken to mean it for
            // everything, rather than discarding a usable answer on a label.
            .or_else(|| quote.fees.first())?;

        let FeeAmount { satoshis, bytes } = &standard.mining_fee;
        if *bytes == 0 || *satoshis == 0 {
            log::warn!(
                "fee quote gave {} satoshis per {} bytes, which is not a usable rate; \
                 keeping the current one",
                satoshis,
                bytes
            );
            return None;
        }
        Some(satoshis.saturating_mul(1000).div_ceil(*bytes))
    }
}

#[cfg(test)]
mod tests {
    //! The broadcaster against a mock mapi-lite. Response-signature
    //! verification is left ON: the mock signs its envelopes with
    //! `uls-core`'s own signer, the way the real server does, rather than the
    //! test switching verification off to make mocking easier.

    use super::*;
    use crate::test_support::{sample_tx, MAPI_TEST_SIGNER_WIF};
    use serde_json::{json, Value};
    use uls_core::envelope::EnvelopeSigner;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Wraps a payload in a signed JSON envelope, as the server does.
    fn signed(payload: &Value) -> Value {
        let signer = EnvelopeSigner::from_wif(MAPI_TEST_SIGNER_WIF).expect("valid WIF");
        let envelope = signer
            .sign(serde_json::to_string(payload).expect("payload serializes"))
            .expect("signing succeeds");
        serde_json::to_value(envelope).expect("envelope serializes")
    }

    /// A `POST /mapi/txs` response body wrapping one result.
    fn txs_payload(result: Value) -> Value {
        signed(&json!({
            "apiVersion": "1.5.0",
            "currentHighestBlockHash": "0000abc",
            "currentHighestBlockHeight": 812_345,
            "failureCount": 0,
            "minerId": "02abc",
            "timestamp": "2026-09-16T00:00:00Z",
            "txs": [result],
            "txSecondMempoolExpiry": 0,
        }))
    }

    /// A `GET /mapi/feeQuote` response body.
    fn fee_quote_payload() -> Value {
        signed(&json!({
            "apiVersion": "1.5.0",
            "timestamp": "2026-09-16T00:00:00Z",
            "expiryTime": "2026-09-16T00:10:00Z",
            "minerId": "02abc",
            "currentHighestBlockHash": "0000abc",
            "currentHighestBlockHeight": 812_345,
            "fees": [{
                "feeType": "standard",
                "miningFee": { "satoshis": 1, "bytes": 1000 },
                "relayFee": { "satoshis": 1, "bytes": 1000 },
            }],
        }))
    }

    /// One per-transaction success result.
    fn success(txid: &str, description: &str) -> Value {
        json!({
            "returnResult": "success",
            "resultDescription": description,
            "txid": txid,
            "failureRetryable": false,
        })
    }

    fn ok(body: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(body)
    }

    fn config(server: &MockServer) -> MapiLiteConfig {
        MapiLiteConfig::for_base_url(server.uri())
    }

    fn broadcaster(server: &MockServer) -> MapiBroadcaster {
        MapiBroadcaster::new(&config(server)).expect("broadcaster builds")
    }

    /// The detail of an upstream error, or a panic naming what came instead.
    fn upstream_detail(error: BroadcastError) -> String {
        match error {
            BroadcastError::Upstream(detail) => detail,
            other => panic!("expected an upstream error, got {other:?}"),
        }
    }

    /// The detail of an unknown outcome -- and an assertion that it is one,
    /// since the whole point of the variant is that it is handled differently
    /// from a failure.
    fn indeterminate_detail(error: BroadcastError) -> String {
        match error {
            BroadcastError::Indeterminate(detail) => detail,
            other => panic!("expected an indeterminate outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sr_bchn_009_mapi_broadcaster_posts_the_transaction_to_mapi_txs() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        let expected = tx.hash().encode();
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(txs_payload(success(&expected, ""))))
            .expect(1)
            .mount(&server)
            .await;

        let txid = broadcaster(&server)
            .broadcast_tx(&tx)
            .await
            .expect("accepted");
        assert_eq!(txid, expected);

        // The body is a one-element batch carrying the raw transaction hex,
        // and declines the merkle proof the server would otherwise default on.
        let requests = server.received_requests().await.expect("requests recorded");
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).expect("a JSON body");
        let batch = body.as_array().expect("a batch");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0]["rawtx"], tx_as_hexstr(&tx).unwrap());
        assert_eq!(batch[0]["merkleProof"], false);
        assert!(batch[0]["callBackUrl"].is_null());
    }

    #[tokio::test]
    async fn mapi_broadcaster_sends_the_configured_authorization_header() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        // Without the header the mock does not match and the call fails.
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ok(txs_payload(success(&tx.hash().encode(), ""))))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = config(&server);
        config.auth_token = Some("Bearer secret".to_string());
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        broadcaster.broadcast_tx(&tx).await.expect("accepted");
    }

    #[tokio::test]
    async fn mapi_broadcaster_already_known_is_a_success() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(txs_payload(success(
                &tx.hash().encode(),
                "Already known",
            ))))
            .mount(&server)
            .await;

        let txid = broadcaster(&server)
            .broadcast_tx(&tx)
            .await
            .expect("known is fine");
        assert_eq!(txid, tx.hash().encode());
    }

    /// CS-427: mapi-lite reported a `txn-mempool-conflict` with
    /// `failureRetryable: true`, so the caller was told "retry may succeed"
    /// about a double spend that never would. A conflict is in the inputs, and
    /// once another transaction holds them, offering the identical transaction
    /// again cannot work whatever the server says.
    #[tokio::test]
    async fn cs_427_a_conflict_is_final_even_when_the_server_calls_it_retryable() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        let rejection = txs_payload(json!({
            "returnResult": "failure",
            "resultDescription": "Mempool error, retry again later. (details: 258 txn-mempool-conflict)",
            "txid": tx.hash().encode(),
            // exactly what the ticket recorded
            "failureRetryable": true,
            "conflictedWith": [{
                "txid": "1263e7f90ab93a140e5a4102c00dd12227bbbb4f98a53a62469f69521f863a17",
                "size": 100,
                "hex": "00",
            }],
        }));
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(rejection))
            .mount(&server)
            .await;

        match broadcaster(&server)
            .broadcast_tx(&tx)
            .await
            .expect_err("rejected")
        {
            BroadcastError::Rejected {
                description,
                retryable,
            } => {
                assert!(
                    description.contains("txn-mempool-conflict"),
                    "{description}"
                );
                assert!(description.contains("1263e7f9"), "{description}");
                assert!(
                    !retryable,
                    "a conflict cannot be resolved by resubmitting the same transaction"
                );
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    /// The narrowing is only for conflicts. A rejection the server calls
    /// retryable and that names no conflict is still retryable -- mempool
    /// full, say -- and reporting it as final would send a caller away from
    /// something that would have worked.
    #[tokio::test]
    async fn cs_427_a_retryable_rejection_without_a_conflict_stays_retryable() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        let rejection = txs_payload(json!({
            "returnResult": "failure",
            "resultDescription": "Mempool full",
            "txid": tx.hash().encode(),
            "failureRetryable": true,
        }));
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(rejection))
            .mount(&server)
            .await;

        match broadcaster(&server)
            .broadcast_tx(&tx)
            .await
            .expect_err("rejected")
        {
            BroadcastError::Rejected { retryable, .. } => assert!(retryable),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mapi_broadcaster_rejection_carries_description_conflicts_and_retryability() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        let rejection = txs_payload(json!({
            "returnResult": "failure",
            "resultDescription": "txn-mempool-conflict",
            "txid": tx.hash().encode(),
            "failureRetryable": false,
            "conflictedWith": [{ "txid": "deadbeef", "size": 100, "hex": "00" }],
        }));
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(rejection))
            .mount(&server)
            .await;

        let error = broadcaster(&server)
            .broadcast_tx(&tx)
            .await
            .expect_err("rejected");

        match error {
            BroadcastError::Rejected {
                description,
                retryable,
            } => {
                assert!(
                    description.contains("txn-mempool-conflict"),
                    "{description}"
                );
                assert!(description.contains("deadbeef"), "{description}");
                assert!(!retryable);
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mapi_broadcaster_refuses_a_result_count_other_than_one() {
        let server = MockServer::start().await;
        let empty_batch = signed(&json!({
            "apiVersion": "1.5.0",
            "currentHighestBlockHash": "0000abc",
            "currentHighestBlockHeight": 812_345,
            "failureCount": 0,
            "minerId": "02abc",
            "timestamp": "2026-09-16T00:00:00Z",
            "txs": [],
            "txSecondMempoolExpiry": 0,
        }));
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(empty_batch))
            .mount(&server)
            .await;

        let error = broadcaster(&server)
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("no result for our transaction");
        assert!(indeterminate_detail(error).contains("returned 0 results"));
    }

    /// A txid that is not the one we hashed means the transaction on the
    /// network is not the one this service built, so the caller must not be
    /// handed outpoints derived from the local hash.
    #[tokio::test]
    async fn mapi_broadcaster_refuses_a_txid_that_is_not_the_transactions_own() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(txs_payload(success("deadbeef", ""))))
            .mount(&server)
            .await;

        let error = broadcaster(&server)
            .broadcast_tx(&tx)
            .await
            .expect_err("a foreign txid is not our transaction");
        let detail = indeterminate_detail(error);
        assert!(detail.contains("deadbeef"), "{detail}");
        assert!(detail.contains(&tx.hash().encode()), "{detail}");
    }

    /// The configured URL is normalised before the client is built, so a
    /// trailing `/` -- which the configuration documents as tolerated -- does
    /// not become a doubled separator in the request path.
    #[tokio::test]
    async fn mapi_broadcaster_tolerates_a_trailing_slash_on_the_base_url() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(txs_payload(success(&tx.hash().encode(), ""))))
            .expect(1)
            .mount(&server)
            .await;

        let config = MapiLiteConfig::for_base_url(format!("{}/", server.uri()));
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        broadcaster.broadcast_tx(&tx).await.expect("accepted");
    }

    /// The retry budget multiplies the per-request timeout, so the whole
    /// submit carries a deadline of its own and a wedged mapi-lite cannot hold
    /// a `/fund` caller for attempts * timeout.
    #[tokio::test]
    async fn mapi_broadcaster_submit_is_bounded_by_the_total_deadline() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ResponseTemplate::new(500).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        // Six attempts of up to 2s each, plus back-off, is fifteen seconds and
        // more; the deadline ends it at three.
        let mut config = config(&server);
        config.timeout_seconds = 2;
        config.max_retries = 5;
        config.total_timeout_seconds = 3;
        assert!(config.validate().is_ok(), "the test config is a legal one");
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        let started = std::time::Instant::now();
        let error = broadcaster
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("deadline");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline did not fire"
        );
        // Unknown, not failed: the request was cancelled in flight, and a
        // cancelled request says nothing about what the server did with it.
        assert!(indeterminate_detail(error).contains("total_timeout_seconds"));
    }

    #[tokio::test]
    async fn mapi_broadcaster_retries_a_server_error_then_gives_up() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ResponseTemplate::new(500))
            // one attempt plus max_retries
            .expect(2)
            .mount(&server)
            .await;

        let mut config = config(&server);
        config.max_retries = 1;
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        let error = broadcaster
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("exhausted");
        assert!(upstream_detail(error).contains("retries exhausted"));
    }

    /// A connection that was refused delivered nothing, so it is a plain
    /// failure: reporting it as an unknown outcome would have the service
    /// reserve inputs that were demonstrably never spent, and a mapi-lite that
    /// is simply down would eat the wallet.
    #[tokio::test]
    async fn sr_fund_011_an_unreachable_mapi_lite_is_a_failure_not_an_unknown_outcome() {
        // A port nothing is listening on: the connection is refused rather
        // than accepted and left hanging.
        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:1");
        config.timeout_seconds = 2;
        config.max_retries = 0;
        config.total_timeout_seconds = 5;
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        let error = broadcaster
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("nothing is listening");
        assert!(
            matches!(error, BroadcastError::Upstream(_)),
            "a refused connection took nothing, got {error:?}"
        );
    }

    #[tokio::test]
    async fn mapi_broadcaster_rejects_a_tampered_envelope() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        let mut envelope = txs_payload(success(&tx.hash().encode(), ""));
        // Keep the signature, swap the payload underneath it.
        envelope["payload"] = json!("{\"txs\":[]}");
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(envelope))
            .mount(&server)
            .await;

        let error = broadcaster(&server)
            .broadcast_tx(&tx)
            .await
            .expect_err("tampered");
        // Unknown rather than failed: an envelope arrived, so the server acted
        // on the transaction, and this client simply cannot trust what it says
        // about it.
        assert!(indeterminate_detail(error).contains("signature"));
    }

    #[tokio::test]
    async fn sr_bchn_011_mapi_broadcaster_health_check_passes_on_a_fee_quote() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mapi/feeQuote"))
            .respond_with(ok(fee_quote_payload()))
            .expect(1)
            .mount(&server)
            .await;

        broadcaster(&server).health_check().await.expect("healthy");
    }

    #[tokio::test]
    async fn sr_bchn_011_mapi_broadcaster_health_check_fails_on_an_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mapi/feeQuote"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let error = broadcaster(&server)
            .health_check()
            .await
            .expect_err("unhealthy");
        assert!(upstream_detail(error).contains("503"));
    }

    #[tokio::test]
    async fn mapi_broadcaster_health_check_is_bounded_by_its_own_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mapi/feeQuote"))
            .respond_with(ok(fee_quote_payload()).set_delay(Duration::from_secs(3)))
            .mount(&server)
            .await;

        let mut config = config(&server);
        config.health_timeout_seconds = 1;
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        let started = std::time::Instant::now();
        let error = broadcaster
            .health_check()
            .await
            .expect_err("a slow upstream is unhealthy");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "probe did not time out"
        );
        // The variant is beside the point here -- a probe only answers
        // reachable or not -- but it must carry a reason for the log.
        assert!(!error.to_string().is_empty());
    }

    #[tokio::test]
    async fn mapi_broadcaster_is_named_mapi_lite() {
        let server = MockServer::start().await;
        assert_eq!(broadcaster(&server).name(), MAPI_LITE);
    }

    // ---- The retry budget actually running (issue #68) ----

    /// `max_retries` has to mean attempts that happen. Before this, the total
    /// deadline cut the default sequence off after one attempt and part of a
    /// second, so the number in the config was not the number that ran.
    ///
    /// wiremock verifies the expectation on drop, so the count is the test.
    #[tokio::test]
    async fn sr_fund_013_every_configured_attempt_is_actually_made() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ResponseTemplate::new(500))
            .expect(3) // one attempt plus max_retries = 2
            .mount(&server)
            .await;

        let config = config(&server);
        assert_eq!(config.max_retries, 2, "the default this test is about");
        assert_eq!(
            config.attempts_within_deadline(),
            3,
            "the defaults must allow all three"
        );

        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");
        broadcaster
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("every attempt is a 500");
    }

    /// The deadline is still the backstop. With attempts that cannot fit, the
    /// sequence stops at the deadline rather than running to the budget.
    #[tokio::test]
    async fn sr_fund_013_the_deadline_still_cuts_a_budget_that_cannot_fit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ResponseTemplate::new(500).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        let mut config = config(&server);
        config.timeout_seconds = 2;
        config.max_retries = 10;
        config.total_timeout_seconds = 3;
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        let started = std::time::Instant::now();
        let error = broadcaster
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("the deadline fires");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "eleven 2s attempts ran instead of stopping at the 3s deadline"
        );
        assert!(indeterminate_detail(error).contains("total_timeout_seconds"));
    }

    /// A retry after a timeout is how an unknown outcome becomes a known one:
    /// mapi-lite answers a transaction it already holds with success, so the
    /// second attempt settles what the first left open. Without the retry the
    /// caller would be told the outcome is unknown and the client's inputs
    /// would be reserved for nothing.
    #[tokio::test]
    async fn sr_fund_013_a_retry_resolves_an_attempt_that_timed_out() {
        let server = MockServer::start().await;
        let tx = sample_tx();
        // first attempt: accepted but far too slow to answer
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(
                ok(txs_payload(success(&tx.hash().encode(), "")))
                    .set_delay(Duration::from_secs(30)),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // second attempt: "already known", which mapi-lite reports as success
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ok(txs_payload(success(
                &tx.hash().encode(),
                "Transaction already known",
            ))))
            .mount(&server)
            .await;

        let mut config = config(&server);
        config.timeout_seconds = 1;
        config.max_retries = 2;
        config.total_timeout_seconds = 10;
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        let txid = broadcaster
            .broadcast_tx(&tx)
            .await
            .expect("the retry settles it");
        assert_eq!(txid, tx.hash().encode());
    }

    /// Once an attempt may have been delivered, a later transport failure
    /// cannot take that back. The call has to stay an unknown outcome, or the
    /// service would leave inputs spendable that may already be spent.
    #[tokio::test]
    async fn sr_fund_013_a_timeout_then_a_failure_is_still_an_unknown_outcome() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Everything after the first attempt is an undecodable answer, which
        // on its own would be one thing; what matters is that the first
        // attempt already put the transaction in doubt.
        Mock::given(method("POST"))
            .and(path("/mapi/txs"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let mut config = config(&server);
        config.timeout_seconds = 1;
        config.max_retries = 1;
        config.total_timeout_seconds = 5;
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        let error = broadcaster
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("nothing succeeded");
        let detail = indeterminate_detail(error);
        assert!(
            detail.contains("earlier attempt may have been delivered"),
            "{detail}"
        );
    }

    /// The counter-case, and the one that caught me out while fixing #66: a
    /// mapi-lite that refuses connections fails fast, so the deadline can be
    /// consumed by back-off alone. That is still a plain failure -- nothing
    /// was ever delivered -- and reporting it as unknown would reserve inputs
    /// that were never spent.
    #[tokio::test]
    async fn sr_fund_013_backoff_exhausting_the_deadline_is_a_failure_not_unknown() {
        // Nothing is listening, so every attempt is refused immediately and
        // only the back-off takes any time.
        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:1");
        config.timeout_seconds = 5;
        config.max_retries = 5;
        config.total_timeout_seconds = 5;
        let broadcaster = MapiBroadcaster::new(&config).expect("broadcaster builds");

        let error = broadcaster
            .broadcast_tx(&sample_tx())
            .await
            .expect_err("nothing is listening");
        assert!(
            matches!(error, BroadcastError::Upstream(_)),
            "a refused connection delivered nothing, got {error:?}"
        );
    }
}
