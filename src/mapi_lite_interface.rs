//! Chain reads through mapi-lite (CS-601).
//!
//! With `[blockchain_interface] interface_type = "mapi_lite"`, the UTXO set and
//! balance of each client come from mapi-lite's `/query/address/*` reads,
//! through `uls-client`, instead of this service holding a WhatsOnChain client
//! of its own. mapi-lite then makes every WhatsOnChain request for the
//! deployment, paced by one limit against one quota, where before this service
//! and mapi-lite each paced their own against the same quota.
//!
//! mapi-lite serves those reads from WhatsOnChain, which indexes mainnet and
//! testnet only, so this backend is refused for regtest and STN at startup;
//! `rpc` remains the way to reach a private chain. Broadcasts do not come here:
//! this backend requires `[mapi_lite]`, so they go through
//! [`crate::broadcaster::mapi::MapiBroadcaster`].

use std::time::Duration;

use async_trait::async_trait;
use chain_gang::interface::blockchain_interface::UNCONFIRMED_HEIGHT;
use chain_gang::interface::{Balance, BlockchainInterface, Utxo, UtxoEntry};
use chain_gang::messages::{BlockHeader, Tx};
use chain_gang::network::Network;
use chain_gang::util::{ChainGangError, Serializable};
use uls_client::{ClientError, MapiClient};

use crate::config::MapiLiteConfig;

/// Reads chain state from a mapi-lite server.
pub struct MapiLiteInterface {
    client: MapiClient,
    network: Network,
}

impl MapiLiteInterface {
    /// A reader for the server `config` names. `timeout`, when set, bounds each
    /// HTTP request; the caller bounds each whole call as well (CS-502), and
    /// one call here is one request.
    pub fn new(config: &MapiLiteConfig, timeout: Option<Duration>) -> Result<Self, String> {
        let mut http = reqwest::Client::builder();
        if let Some(timeout) = timeout {
            http = http.timeout(timeout);
        }
        let http = http
            .build()
            .map_err(|e| format!("Unable to build the mapi-lite HTTP client: {e}"))?;
        // Only mapi-lite's own keys are believed (CS-523), as for broadcasts.
        let mut client = MapiClient::new(config.base_url().to_string())
            .with_http(http)
            .with_trusted_miner_ids(&config.miner_id_public_keys);
        if let Some(token) = &config.auth_token {
            client = client.with_token(token.clone());
        }
        Ok(MapiLiteInterface {
            client,
            network: Network::BSV_Testnet,
        })
    }
}

/// A `uls-client` failure as chain-gang's error. Every one of them is a failed
/// read, which the service turns into `chain_unavailable` and never into an
/// empty UTXO set; a WhatsOnChain that is busy or not configured on mapi-lite's
/// side arrives here as its `503`.
fn read_failed(what: &str, error: ClientError) -> ChainGangError {
    ChainGangError::ResponseError(format!("mapi-lite {what}: {error}"))
}

/// mapi-lite's height for a UTXO, in chain-gang's terms: `0` is mapi-lite's
/// "unconfirmed" (`docs/API.md` §14), where chain-gang and this service use a
/// negative height. Height 0 cannot be a confirmation: the genesis output is
/// unspendable.
fn chain_gang_height(height: i64) -> Result<i32, ChainGangError> {
    if height <= 0 {
        return Ok(UNCONFIRMED_HEIGHT);
    }
    i32::try_from(height)
        .map_err(|_| ChainGangError::BadData(format!("mapi-lite reported height {height}")))
}

#[async_trait]
impl BlockchainInterface for MapiLiteInterface {
    fn set_network(&mut self, network: &Network) {
        self.network = *network;
    }

    /// Whether mapi-lite answers, authenticated and with a signature from one
    /// of its own keys: the fee quote is the cheapest request that proves all
    /// three, and costs mapi-lite no WhatsOnChain request.
    async fn status(&self) -> Result<(), ChainGangError> {
        self.client
            .fee_quote()
            .await
            .map(|_| ())
            .map_err(|e| read_failed("status", e))
    }

    async fn get_balance(&self, address: &str) -> Result<Balance, ChainGangError> {
        let balance = self
            .client
            .address_balance(address)
            .await
            .map_err(|e| read_failed("address balance", e))?;
        Ok(Balance {
            confirmed: balance.confirmed_satoshis,
            unconfirmed: balance.unconfirmed_satoshis,
        })
    }

    /// The address's UTXO set. mapi-lite follows WhatsOnChain's pagination
    /// and leaves out outputs already spent in the mempool, as chain-gang's
    /// WhatsOnChain interface does, so the set means the same either way.
    async fn get_utxo(&self, address: &str) -> Result<Utxo, ChainGangError> {
        let utxos = self
            .client
            .address_utxos(address)
            .await
            .map_err(|e| read_failed("address UTXOs", e))?;
        utxos
            .utxos
            .into_iter()
            .map(|u| {
                Ok(UtxoEntry {
                    height: chain_gang_height(u.height)?,
                    tx_pos: u.vout,
                    tx_hash: u.txid,
                    value: u.value_satoshis,
                })
            })
            .collect()
    }

    /// Not served here: this backend requires `[mapi_lite]`, so every
    /// broadcast goes through `MapiBroadcaster`, with its retries and its
    /// unknown-outcome handling.
    async fn broadcast_tx(&self, _tx: &Tx) -> Result<String, ChainGangError> {
        Err(ChainGangError::BadArgument(
            "broadcasts go through MapiBroadcaster, not the chain-read interface".to_string(),
        ))
    }

    async fn get_tx(&self, txid: &str) -> Result<Tx, ChainGangError> {
        let bytes = self
            .client
            .query_transaction_bytes(txid)
            .await
            .map_err(|e| read_failed("transaction", e))?;
        Tx::read(&mut bytes.as_slice())
    }

    /// mapi-lite publishes no headers, and this service never asks for one.
    async fn get_latest_block_header(&self) -> Result<BlockHeader, ChainGangError> {
        Err(ChainGangError::BadArgument(
            "mapi-lite does not serve block headers".to_string(),
        ))
    }

    /// The service calls this only to learn whether the chain source answers
    /// (`/status`), so it answers with mapi-lite's chain tip from the fee
    /// quote rather than headers mapi-lite does not publish.
    async fn get_block_headers(&self) -> Result<String, ChainGangError> {
        let quote = self
            .client
            .fee_quote()
            .await
            .map_err(|e| read_failed("chain tip", e))?;
        Ok(serde_json::json!({
            "hash": quote.current_highest_block_hash,
            "height": quote.current_highest_block_height,
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    //! Against a mock mapi-lite serving the real wire shapes, its envelopes
    //! signed with `uls-core`'s own signer.

    use super::*;
    use crate::test_support::MAPI_TEST_SIGNER_WIF;
    use serde_json::{json, Value};
    use uls_core::envelope::EnvelopeSigner;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ADDRESS: &str = "mzLc1yZpS8bLY8kXsRsL3Ff3J9uq5jo7wD";

    fn signed(payload: &Value) -> Value {
        let signer = EnvelopeSigner::from_wif(MAPI_TEST_SIGNER_WIF).unwrap();
        serde_json::to_value(signer.sign(payload.to_string()).unwrap()).unwrap()
    }

    fn fee_quote() -> Value {
        signed(&json!({
            "apiVersion": "1.5.0", "timestamp": "2026-10-14T00:00:00Z",
            "expiryTime": "2026-10-14T00:10:00Z", "minerId": "02abc",
            "currentHighestBlockHash": "00ff", "currentHighestBlockHeight": 1_700_000,
            "fees": []
        }))
    }

    fn interface(server: &MockServer) -> MapiLiteInterface {
        let mut config = MapiLiteConfig::for_base_url(server.uri());
        config.auth_token = Some("Bearer t".to_string());
        MapiLiteInterface::new(&config, Some(Duration::from_secs(5))).unwrap()
    }

    #[tokio::test]
    async fn sr_bchn_019_utxos_come_from_mapi_lite_with_unconfirmed_as_negative_height() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/query/address/{ADDRESS}/utxos")))
            .and(header("authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "address": ADDRESS,
                "utxos": [
                    {"txid": "aa".repeat(32), "vout": 1, "value_satoshis": 5_000, "height": 1_699_990},
                    {"txid": "bb".repeat(32), "vout": 0, "value_satoshis": 700, "height": 0},
                ]
            })))
            .mount(&server)
            .await;

        let utxo = interface(&server).get_utxo(ADDRESS).await.unwrap();
        assert_eq!(
            utxo,
            vec![
                UtxoEntry {
                    height: 1_699_990,
                    tx_pos: 1,
                    tx_hash: "aa".repeat(32),
                    value: 5_000
                },
                UtxoEntry {
                    height: UNCONFIRMED_HEIGHT,
                    tx_pos: 0,
                    tx_hash: "bb".repeat(32),
                    value: 700
                },
            ]
        );
    }

    /// mapi-lite answers 503 when its WhatsOnChain is busy or not configured:
    /// a failed read, never an empty set that would look like an empty wallet.
    #[tokio::test]
    async fn sr_bchn_019_a_busy_or_unconfigured_whatsonchain_is_a_failed_read() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/query/address/{ADDRESS}/utxos")))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let error = interface(&server).get_utxo(ADDRESS).await.unwrap_err();
        assert!(error.to_string().contains("503"), "{error}");
    }

    #[tokio::test]
    async fn sr_bchn_019_balance_comes_from_mapi_lite() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/query/address/{ADDRESS}/balance")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "address": ADDRESS, "confirmed_satoshis": 9_000, "unconfirmed_satoshis": -300
            })))
            .mount(&server)
            .await;

        let balance = interface(&server).get_balance(ADDRESS).await.unwrap();
        assert_eq!((balance.confirmed, balance.unconfirmed), (9_000, -300));
    }

    /// `status` and `/status`'s connectivity check ask the signed fee quote,
    /// which costs mapi-lite no WhatsOnChain request.
    #[tokio::test]
    async fn sr_bchn_019_status_and_the_chain_tip_come_from_the_fee_quote() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mapi/feeQuote"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fee_quote()))
            .mount(&server)
            .await;

        let interface = interface(&server);
        interface.status().await.unwrap();
        let tip: Value =
            serde_json::from_str(&interface.get_block_headers().await.unwrap()).unwrap();
        assert_eq!(tip["height"], 1_700_000);
    }

    #[tokio::test]
    async fn sr_bchn_019_an_unreachable_mapi_lite_fails_status() {
        // A port bound and released here, so nothing listens on it. Not a
        // dropped MockServer: wiremock pools its servers, and a dropped one
        // can still be listening, answering for another test.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let config = MapiLiteConfig::for_base_url(format!("http://127.0.0.1:{port}"));
        let interface = MapiLiteInterface::new(&config, Some(Duration::from_secs(2))).unwrap();
        assert!(interface.status().await.is_err());
    }

    /// The service's own chain refresh, on this backend: the client's
    /// balance is what mapi-lite reports for its address, unconfirmed included.
    #[tokio::test]
    async fn sr_bchn_019_a_client_balance_is_read_through_mapi_lite() {
        use crate::service::Service;
        use crate::test_support::{
            test_config, unique_dynamic_config_path, TEST_ADDRESS, TEST_CLIENT_ID,
        };
        use std::sync::Arc;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/mapi/feeQuote"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fee_quote()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/query/address/{TEST_ADDRESS}/utxos")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "address": TEST_ADDRESS,
                "utxos": [
                    {"txid": "aa".repeat(32), "vout": 0, "value_satoshis": 12_000, "height": 1_699_000},
                    {"txid": "bb".repeat(32), "vout": 1, "value_satoshis": 500, "height": 0},
                ]
            })))
            .mount(&server)
            .await;

        let config = test_config(&unique_dynamic_config_path());
        let mut reader = interface(&server);
        reader.set_network(&config.get_network().unwrap());
        let reader: Arc<dyn BlockchainInterface + Send + Sync> = Arc::new(reader);
        let service = Arc::new(Service::new_for_test(&config, reader).await);

        Service::refresh_client_chain_state(&service, TEST_CLIENT_ID)
            .await
            .expect("the chain refresh reads through mapi-lite");
        let balance = service.get_balance(TEST_CLIENT_ID).await.unwrap();
        assert_eq!((balance.confirmed, balance.unconfirmed), (12_000, 500));
    }

    #[test]
    fn mapi_lite_heights_map_to_chain_gangs() {
        assert_eq!(chain_gang_height(0).unwrap(), UNCONFIRMED_HEIGHT);
        assert_eq!(chain_gang_height(-1).unwrap(), UNCONFIRMED_HEIGHT);
        assert_eq!(chain_gang_height(800_000).unwrap(), 800_000);
        assert!(chain_gang_height(i64::from(i32::MAX) + 1).is_err());
    }

    #[tokio::test]
    async fn broadcasts_are_not_served_by_the_read_interface() {
        let server = MockServer::start().await;
        let error = interface(&server)
            .broadcast_tx(&crate::test_support::sample_tx())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("MapiBroadcaster"), "{error}");
    }
}
