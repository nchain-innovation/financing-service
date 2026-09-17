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
//! Two clients share one base URL and token: the submit client carries the
//! configured request timeout and retry budget, the probe client a short
//! timeout and no retries, because `GET /health` has to answer inside the
//! Docker health check's three seconds however slow mapi-lite is being.
//!
//! A per-request timeout bounds one attempt but not the retry sequence, so a
//! submit also carries a total deadline (`mapi_lite.total_timeout_seconds`).
//! A funding call is interactive: a wedged mapi-lite must not hold the caller,
//! and the actix worker serving it, for `timeout * (max_retries + 1)`.

use std::time::Duration;

use async_trait::async_trait;
use chain_gang::messages::Tx;
use uls_client::{ClientError, MapiClient, SubmitTxRequest};
use uls_core::status::RESULT_SUCCESS;

use super::{BroadcastError, TxBroadcaster, MAPI_LITE};
use crate::{config::MapiLiteConfig, util::tx_as_hexstr};

/// Wait between submit retries: linear back-off, `attempt * BASE` capped at
/// `CAP`. uls-client's own default caps at 60s, which would hold a `/fund`
/// request open for minutes; a funding call is interactive.
const RETRY_BACKOFF_BASE: Duration = Duration::from_secs(1);
const RETRY_BACKOFF_CAP: Duration = Duration::from_secs(5);

/// Hands transactions to a mapi-lite server.
pub struct MapiBroadcaster {
    submit: MapiClient,
    probe: MapiClient,
    /// Ceiling on a whole submit, retries and back-off included. The
    /// per-request timeout bounds one attempt; this bounds the sequence, so a
    /// `/fund` caller cannot be held for `timeout * (max_retries + 1)`.
    submit_deadline: Duration,
}

impl MapiBroadcaster {
    /// Build a broadcaster for the server named by `config`.
    pub fn new(config: &MapiLiteConfig) -> Result<Self, String> {
        let submit = Self::client(config, config.timeout())?.retry(
            config.max_retries,
            RETRY_BACKOFF_BASE,
            RETRY_BACKOFF_CAP,
        );
        // fee_quote does not retry, so the probe needs no retry settings.
        let probe = Self::client(config, config.health_timeout())?;
        Ok(Self {
            submit,
            probe,
            submit_deadline: config.total_timeout(),
        })
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
    fn from(error: ClientError) -> Self {
        BroadcastError::Upstream(error.to_string())
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

        // The retry budget multiplies the per-request timeout, so the whole
        // sequence gets a ceiling of its own: against a mapi-lite that accepts
        // connections and never answers, the caller and the worker serving it
        // are freed at the deadline rather than at attempts * timeout.
        let payload = match tokio::time::timeout(
            self.submit_deadline,
            self.submit.submit_transactions(&[request]),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Err(BroadcastError::Upstream(format!(
                    "mapi-lite did not answer within {}s (mapi_lite.total_timeout_seconds)",
                    self.submit_deadline.as_secs()
                )))
            }
        };

        // One in, one out. Anything else means the server and this client
        // disagree about the batch contract, and guessing which result is
        // ours would be worse than refusing.
        let [result] = payload.txs.as_slice() else {
            return Err(BroadcastError::Upstream(format!(
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
            return Err(BroadcastError::Rejected {
                description,
                retryable: result.failure_retryable,
            });
        }

        // "Already known" also comes back as success: a resubmitted funding
        // transaction is the same transaction, so that is the right answer.
        //
        // The txid must be the one we hashed. A different one means what
        // reached the network is not the transaction this service built, and
        // the caller would otherwise be handed outpoints -- derived from the
        // local hash -- for a transaction that will never confirm, with its
        // UTXOs already marked spent. Refused for the same reason the batch
        // length is: a disagreement here is not ours to guess through.
        let expected = tx.hash().encode();
        if result.txid != expected {
            return Err(BroadcastError::Upstream(format!(
                "mapi-lite reported txid {} for a funding transaction hashing to {expected}",
                result.txid
            )));
        }
        Ok(result.txid.clone())
    }

    async fn health_check(&self) -> Result<(), BroadcastError> {
        self.probe.fee_quote().await?;
        Ok(())
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
        assert!(upstream_detail(error).contains("returned 0 results"));
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
        let detail = upstream_detail(error);
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
        assert!(upstream_detail(error).contains("total_timeout_seconds"));
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
        assert!(upstream_detail(error).contains("signature"));
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
        assert!(!upstream_detail(error).is_empty());
    }

    #[tokio::test]
    async fn mapi_broadcaster_is_named_mapi_lite() {
        let server = MockServer::start().await;
        assert_eq!(broadcaster(&server).name(), MAPI_LITE);
    }
}
