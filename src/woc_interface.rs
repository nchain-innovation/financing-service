//! Blockchain interfaces for the WhatsOnChain HTTP API.
//!
//! Two deployments of that API matter here, and they do not serve the same
//! paths. [`WocInterface`] is the trait that captures the difference: an
//! implementation says where its instance lives and which URLs to ask, and
//! [`WocClient`] does the HTTP work and adapts the result to chain-gang's
//! [`BlockchainInterface`].
//!
//! | | [`WocPublicInterface`] | [`WocLocalInterface`] |
//! | --- | --- | --- |
//! | base | `https://api.whatsonchain.com/v1/bsv/{net}` | `[blockchain_interface] url` |
//! | status | `/woc` | `/woc` |
//! | balance | `/address/{a}/balance` | `/address/{a}/confirmed/balance` **and** `/address/{a}/unconfirmed/balance` |
//! | utxo | `/address/{a}/unspent` | `/address/{a}/unspent/all`, wrapped in `{"result": [...]}`, with mempool duplicates |
//! | broadcast | `/tx/raw` | `/tx/raw` |
//! | tx | `/tx/{txid}/hex` | `/tx/{txid}/hex` |
//! | latest header | `/block/headers/latest?count=1` | `/block/headers/latest` |
//! | headers | `/block/headers` | `/block/headers` |
//!
//! Two woc-api quirks in the utxo response are handled in `parse_utxo`:
//! unconfirmed entries omit `height` altogether, and an outpoint that is both
//! in the mempool and in a block is listed twice.
//!
//! The self-hosted woc-api (the `woc` Docker profile, see docker/README.md)
//! serves a single network, so it has no `/v1/bsv/{net}` prefix to put one in.
//! Its UTXO entries are field-compatible with chain-gang's `UtxoEntry`, so only
//! the wrapper has to be peeled off.

use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use chain_gang::{
    interface::{Balance, BlockchainInterface, Utxo, UtxoEntry},
    messages::{BlockHeader, Tx},
    network::Network,
    util::{ChainGangError, Serializable},
};

/// Host serving the public WhatsOnChain API.
const PUBLIC_WOC_HOST: &str = "https://api.whatsonchain.com";

/// Body for `POST /tx/raw`.
#[derive(Debug, Serialize)]
struct BroadcastTxType {
    pub txhex: String,
}

/// woc-api's `/address/{address}/confirmed/balance`.
#[derive(Debug, Deserialize)]
struct ConfirmedBalance {
    #[serde(default)]
    confirmed: i64,
    #[serde(default)]
    error: String,
}

/// woc-api's `/address/{address}/unconfirmed/balance`.
#[derive(Debug, Deserialize)]
struct UnconfirmedBalance {
    #[serde(default)]
    unconfirmed: i64,
    #[serde(default)]
    error: String,
}

/// One entry of woc-api's `unspent/all` result.
///
/// Deliberately not chain-gang's `UtxoEntry`: woc-api omits `height` entirely
/// while an output is unconfirmed, and `UtxoEntry::height` is a required
/// `i32`, so deserialising straight into it fails with
/// "missing field `height`" as soon as anything is in the mempool.
#[derive(Debug, Deserialize)]
struct UnspentEntry {
    /// Absent while unconfirmed. 0 is what public WhatsOnChain reports for
    /// mempool entries, and nothing in this service branches on height.
    #[serde(default)]
    height: i32,
    tx_pos: u32,
    tx_hash: String,
    value: i64,
}

impl From<UnspentEntry> for UtxoEntry {
    fn from(entry: UnspentEntry) -> Self {
        UtxoEntry {
            height: entry.height,
            tx_pos: entry.tx_pos,
            tx_hash: entry.tx_hash,
            value: entry.value,
        }
    }
}

/// woc-api's `/address/{address}/unspent/all`.
#[derive(Debug, Deserialize)]
struct UnspentAll {
    #[serde(default)]
    result: Vec<UnspentEntry>,
    #[serde(default)]
    error: String,
}

/// Deserialise a response body, logging it when it does not parse.
fn parse_json<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, ChainGangError> {
    serde_json::from_str(body).map_err(|e| {
        log::warn!("txt = {}", body);
        ChainGangError::JSONParseError(format!("json parse error = {}", e))
    })
}

fn unexpected_bodies(what: &str, got: usize) -> ChainGangError {
    ChainGangError::ResponseError(format!("{what}: unexpected number of responses = {got}"))
}

/// One deployment of the WhatsOnChain HTTP API.
///
/// Implementations describe *where* to ask and *what shape* comes back;
/// [`WocClient`] owns the requests themselves. Everything here is pure, so the
/// URL layout of an instance can be unit tested without a server.
pub trait WocInterface: Send + Sync + fmt::Debug {
    /// Root that every path below hangs off, with no trailing slash.
    ///
    /// The network is passed in because a multi-network instance encodes it in
    /// the base URL; a single-network one ignores it.
    fn base_url(&self, network: Network) -> Result<String, ChainGangError>;

    /// Requests needed to answer a balance query, in the order
    /// [`WocInterface::parse_balance`] expects their bodies.
    fn balance_paths(&self, address: &str) -> Vec<String>;

    /// Fold the bodies of [`WocInterface::balance_paths`] into a `Balance`.
    fn parse_balance(&self, bodies: &[String]) -> Result<Balance, ChainGangError>;

    /// Request answering an unspent-outputs query.
    fn utxo_path(&self, address: &str) -> String;

    /// Decode the body of [`WocInterface::utxo_path`].
    fn parse_utxo(&self, body: &str) -> Result<Utxo, ChainGangError>;

    /// Request returning the most recent block header as hex.
    fn latest_block_header_path(&self) -> String;

    /// Liveness probe. Both deployments answer with `Whats On Chain`.
    fn status_path(&self) -> String {
        "/woc".to_string()
    }

    /// Endpoint a raw transaction is POSTed to.
    fn broadcast_tx_path(&self) -> String {
        "/tx/raw".to_string()
    }

    /// Request returning a transaction as hex.
    fn tx_hex_path(&self, txid: &str) -> String {
        format!("/tx/{txid}/hex")
    }

    /// Request returning the block headers.
    fn block_headers_path(&self) -> String {
        "/block/headers".to_string()
    }
}

/// Public WhatsOnChain at <https://api.whatsonchain.com>.
///
/// Paths carry a `/v1/bsv/{network}` prefix, and the instance serves every BSV
/// network, so the configured network selects which one is addressed.
#[derive(Debug, Clone, Copy, Default)]
pub struct WocPublicInterface;

impl WocPublicInterface {
    pub fn new() -> Self {
        WocPublicInterface
    }
}

impl WocInterface for WocPublicInterface {
    fn base_url(&self, network: Network) -> Result<String, ChainGangError> {
        let network = match network {
            Network::BSV_Mainnet => "main",
            Network::BSV_Testnet => "test",
            Network::BSV_STN => "stn",
            other => {
                return Err(ChainGangError::ResponseError(format!(
                    "WhatsOnChain does not serve network {other}"
                )));
            }
        };
        Ok(format!("{PUBLIC_WOC_HOST}/v1/bsv/{network}"))
    }

    fn balance_paths(&self, address: &str) -> Vec<String> {
        vec![format!("/address/{address}/balance")]
    }

    fn parse_balance(&self, bodies: &[String]) -> Result<Balance, ChainGangError> {
        let [balance] = bodies else {
            return Err(unexpected_bodies("balance", bodies.len()));
        };
        parse_json(balance)
    }

    fn utxo_path(&self, address: &str) -> String {
        format!("/address/{address}/unspent")
    }

    fn parse_utxo(&self, body: &str) -> Result<Utxo, ChainGangError> {
        parse_json(body)
    }

    fn latest_block_header_path(&self) -> String {
        "/block/headers/latest?count=1".to_string()
    }
}

/// A self-hosted woc-api instance.
///
/// It serves one network, so its paths have no `/v1/bsv/{network}` prefix and
/// the configured network does not enter the URL.
#[derive(Debug, Clone)]
pub struct WocLocalInterface {
    /// Base URL with any trailing slash removed, e.g. `http://woc-api:8084`.
    base_url: String,
}

impl WocLocalInterface {
    pub fn new(base_url: &str) -> Self {
        WocLocalInterface {
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

impl WocInterface for WocLocalInterface {
    fn base_url(&self, _network: Network) -> Result<String, ChainGangError> {
        Ok(self.base_url.clone())
    }

    /// Self-hosted woc-api splits this across two endpoints, so this is two
    /// requests where public WhatsOnChain needs one.
    fn balance_paths(&self, address: &str) -> Vec<String> {
        vec![
            format!("/address/{address}/confirmed/balance"),
            format!("/address/{address}/unconfirmed/balance"),
        ]
    }

    fn parse_balance(&self, bodies: &[String]) -> Result<Balance, ChainGangError> {
        let [confirmed, unconfirmed] = bodies else {
            return Err(unexpected_bodies("balance", bodies.len()));
        };

        let confirmed: ConfirmedBalance = parse_json(confirmed)?;
        if !confirmed.error.is_empty() {
            return Err(ChainGangError::ResponseError(format!(
                "confirmed balance error = {}",
                confirmed.error
            )));
        }

        let unconfirmed: UnconfirmedBalance = parse_json(unconfirmed)?;
        if !unconfirmed.error.is_empty() {
            return Err(ChainGangError::ResponseError(format!(
                "unconfirmed balance error = {}",
                unconfirmed.error
            )));
        }

        Ok(Balance {
            confirmed: confirmed.confirmed,
            unconfirmed: unconfirmed.unconfirmed,
        })
    }

    fn utxo_path(&self, address: &str) -> String {
        format!("/address/{address}/unspent/all")
    }

    fn parse_utxo(&self, body: &str) -> Result<Utxo, ChainGangError> {
        let unspent: UnspentAll = parse_json(body)?;
        if !unspent.error.is_empty() {
            return Err(ChainGangError::ResponseError(format!(
                "unspent error = {}",
                unspent.error
            )));
        }

        // While an output sits in the mempool *and* in a block, woc-api lists
        // the same outpoint twice -- once "unconfirmed" with no height, once
        // "confirmed" with one. Taking both would double the apparent balance
        // and build a transaction that spends one outpoint twice, so keep a
        // single entry per outpoint, preferring the confirmed view.
        let mut utxo: Utxo = Vec::with_capacity(unspent.result.len());
        let mut seen: HashMap<(String, u32), usize> = HashMap::new();
        for entry in unspent.result {
            let key = (entry.tx_hash.clone(), entry.tx_pos);
            let candidate: UtxoEntry = entry.into();
            match seen.get(&key) {
                Some(&index) => {
                    if utxo[index].height == 0 && candidate.height != 0 {
                        utxo[index] = candidate;
                    }
                }
                None => {
                    seen.insert(key, utxo.len());
                    utxo.push(candidate);
                }
            }
        }
        Ok(utxo)
    }

    fn latest_block_header_path(&self) -> String {
        "/block/headers/latest".to_string()
    }
}

/// A [`BlockchainInterface`] speaking the WhatsOnChain API to whichever
/// instance `W` describes.
#[derive(Debug, Clone)]
pub struct WocClient<W: WocInterface> {
    woc: W,
    network_type: Network,
}

impl<W: WocInterface> WocClient<W> {
    pub fn new(woc: W) -> Self {
        WocClient {
            woc,
            network_type: Network::BSV_Testnet,
        }
    }

    fn url(&self, path: &str) -> Result<String, ChainGangError> {
        Ok(format!("{}{}", self.woc.base_url(self.network_type)?, path))
    }

    /// GET `path`, returning the body only on HTTP 200.
    async fn get_text(&self, path: &str) -> Result<String, ChainGangError> {
        let url = self.url(path)?;
        // NOTE: chain-gang builds against reqwest 0.13 while this crate uses
        // 0.12, so the two `reqwest::Error` types are distinct and
        // ChainGangError's `From<reqwest::Error>` does not apply here. Every
        // reqwest error therefore has to be mapped by hand.
        let response = reqwest::get(&url)
            .await
            .map_err(|e| ChainGangError::ResponseError(format!("request failed = {}", e)))?;
        if response.status() != StatusCode::OK {
            log::warn!("url = {}", url);
            return Err(ChainGangError::ResponseError(format!(
                "response.status() = {}",
                response.status()
            )));
        }
        response
            .text()
            .await
            .map_err(|e| ChainGangError::ResponseError(format!("response.text() = {}", e)))
    }
}

#[async_trait]
impl<W: WocInterface> BlockchainInterface for WocClient<W> {
    fn set_network(&mut self, network: &Network) {
        self.network_type = *network;
    }

    /// Return Ok(()) if connection is good.
    async fn status(&self) -> Result<(), ChainGangError> {
        log::debug!("status");
        match self.get_text(&self.woc.status_path()).await?.as_str() {
            "Whats On Chain" => Ok(()),
            txt => Err(ChainGangError::ResponseError(format!(
                "Unexpected txt = {}",
                txt
            ))),
        }
    }

    /// Get balance associated with address.
    async fn get_balance(&self, address: &str) -> Result<Balance, ChainGangError> {
        log::debug!("get_balance");
        let mut bodies = Vec::new();
        for path in self.woc.balance_paths(address) {
            bodies.push(self.get_text(&path).await?);
        }
        self.woc.parse_balance(&bodies)
    }

    /// Get UTXO associated with address.
    async fn get_utxo(&self, address: &str) -> Result<Utxo, ChainGangError> {
        log::debug!("get_utxo");
        let body = self.get_text(&self.woc.utxo_path(address)).await?;
        self.woc.parse_utxo(&body)
    }

    /// Broadcast Tx, return the txid.
    async fn broadcast_tx(&self, tx: &Tx) -> Result<String, ChainGangError> {
        log::debug!("broadcast_tx");
        let url = self.url(&self.woc.broadcast_tx_path())?;
        let data_for_broadcast = BroadcastTxType {
            txhex: tx.as_hexstr(),
        };
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .json(&data_for_broadcast)
            .send()
            .await
            .map_err(|e| ChainGangError::ResponseError(format!("request failed = {}", e)))?;
        let status = response.status();
        match status {
            StatusCode::OK => {
                let res = response.text().await.map_err(|e| {
                    ChainGangError::ResponseError(format!("response.text() = {}", e))
                })?;
                let txid = res.trim().trim_matches('"');
                Ok(txid.to_string())
            }
            _ => {
                log::warn!("url = {}", url);
                Err(ChainGangError::ResponseError(format!(
                    "response.status() = {}",
                    status
                )))
            }
        }
    }

    async fn get_tx(&self, txid: &str) -> Result<Tx, ChainGangError> {
        log::debug!("get_tx");
        let txt = self.get_text(&self.woc.tx_hex_path(txid)).await?;
        let bytes = hex::decode(txt.trim())?;
        let mut byte_slice = &bytes[..];
        Ok(Tx::read(&mut byte_slice)?)
    }

    async fn get_latest_block_header(&self) -> Result<BlockHeader, ChainGangError> {
        log::debug!("get_latest_block_header");
        let path = self.woc.latest_block_header_path();
        let txt = self.get_text(&path).await?;
        let trimmed = txt.trim();
        if trimmed.is_empty() {
            // Self-hosted woc-api answers 200 with an empty body rather than
            // the hex header public WhatsOnChain returns. Nothing in this
            // service calls this method; surface it plainly rather than
            // panicking on an empty hex decode.
            return Err(ChainGangError::ResponseError(format!(
                "{path} returned an empty body -- this instance does not serve \
                 the latest block header as hex"
            )));
        }
        let bytes = hex::decode(trimmed)?;
        let mut byte_slice = &bytes[..];
        Ok(BlockHeader::read(&mut byte_slice)?)
    }

    async fn get_block_headers(&self) -> Result<String, ChainGangError> {
        log::debug!("get_block_headers");
        self.get_text(&self.woc.block_headers_path()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(base_url: &str) -> WocClient<WocLocalInterface> {
        WocClient::new(WocLocalInterface::new(base_url))
    }

    fn public() -> WocClient<WocPublicInterface> {
        WocClient::new(WocPublicInterface::new())
    }

    #[test]
    fn base_url_trailing_slash_is_stripped() {
        let client = local("http://woc-api:8084/");
        assert_eq!(client.url("/woc").unwrap(), "http://woc-api:8084/woc");
    }

    #[test]
    fn base_url_without_trailing_slash_is_unchanged() {
        let client = local("http://woc-api:8084");
        assert_eq!(
            client.url(&client.woc.utxo_path("abc")).unwrap(),
            "http://woc-api:8084/address/abc/unspent/all"
        );
    }

    #[test]
    fn local_paths_carry_no_v1_bsv_network_prefix() {
        // The whole point of this implementation: public WhatsOnChain paths are
        // /v1/bsv/{network}/... -- a self-hosted instance serves them bare.
        let client = local("http://localhost:5010");
        assert!(!client.url("/woc").unwrap().contains("/v1/bsv/"));
    }

    #[test]
    fn public_paths_carry_the_network_prefix() {
        let mut client = public();
        assert_eq!(
            client.url("/woc").unwrap(),
            "https://api.whatsonchain.com/v1/bsv/test/woc"
        );
        client.set_network(&Network::BSV_Mainnet);
        assert_eq!(
            client.url("/woc").unwrap(),
            "https://api.whatsonchain.com/v1/bsv/main/woc"
        );
    }

    #[test]
    fn public_rejects_non_bsv_networks() {
        let mut client = public();
        client.set_network(&Network::BTC_Mainnet);
        assert!(client.url("/woc").is_err());
    }

    #[test]
    fn balance_is_one_request_publicly_and_two_locally() {
        assert_eq!(
            WocPublicInterface::new().balance_paths("mm7J"),
            vec!["/address/mm7J/balance"]
        );
        assert_eq!(
            WocLocalInterface::new("http://localhost:5010").balance_paths("mm7J"),
            vec![
                "/address/mm7J/confirmed/balance",
                "/address/mm7J/unconfirmed/balance",
            ]
        );
    }

    /// The exact body that produced
    /// `JSONParseError("missing field `height` at line 1 column 349")` from a
    /// live regtest node: a mempool entry with no `height`, alongside the
    /// confirmed view of the very same outpoint.
    #[test]
    fn local_unspent_all_tolerates_mempool_entries_without_height() {
        let body = r#"{"address":"mirdE7PihEkLD3NVT7XoqaVrf7jVo7heVQ","script":"0d16","result":[
            {"tx_pos":0,"tx_hash":"6eafff5d","value":99999150,"isSpentInMempoolTx":false,"hex":"76a914","status":"unconfirmed"},
            {"height":1187,"tx_pos":0,"tx_hash":"6eafff5d","value":99999150,"isSpentInMempoolTx":false,"status":"confirmed"}
        ],"error":""}"#;
        let utxo = WocLocalInterface::new("http://localhost:5010")
            .parse_utxo(body)
            .unwrap();

        // One outpoint, listed twice by woc-api, must not become two UTXOs --
        // that would double the balance and spend the same outpoint twice.
        assert_eq!(utxo.len(), 1);
        assert_eq!(utxo[0].value, 99999150);
        // The confirmed view wins, whichever order they arrive in.
        assert_eq!(utxo[0].height, 1187);
    }

    #[test]
    fn local_unspent_all_prefers_confirmed_when_it_comes_first() {
        let body = r#"{"address":"mm7J","script":"03","result":[
            {"height":1187,"tx_pos":0,"tx_hash":"6eafff5d","value":99999150,"status":"confirmed"},
            {"tx_pos":0,"tx_hash":"6eafff5d","value":99999150,"status":"unconfirmed"}
        ],"error":""}"#;
        let utxo = WocLocalInterface::new("http://localhost:5010")
            .parse_utxo(body)
            .unwrap();
        assert_eq!(utxo.len(), 1);
        assert_eq!(utxo[0].height, 1187);
    }

    #[test]
    fn local_unspent_all_keeps_a_purely_unconfirmed_output() {
        // A freshly received output that is only in the mempool is still
        // spendable and must survive with height 0.
        let body = r#"{"address":"mm7J","script":"03","result":[
            {"tx_pos":1,"tx_hash":"abcd","value":4200,"status":"unconfirmed"}
        ],"error":""}"#;
        let utxo = WocLocalInterface::new("http://localhost:5010")
            .parse_utxo(body)
            .unwrap();
        assert_eq!(utxo.len(), 1);
        assert_eq!(utxo[0].height, 0);
        assert_eq!(utxo[0].value, 4200);
        assert_eq!(utxo[0].tx_pos, 1);
    }

    #[test]
    fn local_unspent_all_keeps_distinct_outpoints_of_one_tx() {
        // Same tx_hash, different tx_pos -- two genuinely different outputs.
        let body = r#"{"address":"mm7J","script":"03","result":[
            {"height":10,"tx_pos":0,"tx_hash":"abcd","value":100,"status":"confirmed"},
            {"height":10,"tx_pos":1,"tx_hash":"abcd","value":200,"status":"confirmed"}
        ],"error":""}"#;
        let utxo = WocLocalInterface::new("http://localhost:5010")
            .parse_utxo(body)
            .unwrap();
        assert_eq!(utxo.len(), 2);
        assert_eq!(utxo.iter().map(|u| u.value).sum::<i64>(), 300);
    }

    #[test]
    fn local_unspent_all_wrapper_is_unwrapped() {
        let body = r#"{"address":"mm7J","script":"03","result":[
            {"height":2,"tx_pos":0,"tx_hash":"f1d1","value":5000000000,"status":"confirmed"}
        ],"error":""}"#;
        let utxo = WocLocalInterface::new("http://localhost:5010")
            .parse_utxo(body)
            .unwrap();
        assert_eq!(utxo.len(), 1);
        assert_eq!(utxo[0].value, 5000000000);
        assert_eq!(utxo[0].height, 2);
        assert_eq!(utxo[0].tx_hash, "f1d1");
    }

    #[test]
    fn public_utxo_is_a_bare_array() {
        let body = r#"[{"height":2,"tx_pos":0,"tx_hash":"f1d1","value":5000000000}]"#;
        let utxo = WocPublicInterface::new().parse_utxo(body).unwrap();
        assert_eq!(utxo.len(), 1);
        assert_eq!(utxo[0].value, 5000000000);
    }

    #[test]
    fn local_balance_endpoints_deserialise() {
        let balance = WocLocalInterface::new("http://localhost:5010")
            .parse_balance(&[
                r#"{"address":"mm7J","confirmed":165000000000,"error":""}"#.to_string(),
                r#"{"address":"mm7J","unconfirmed":0,"error":""}"#.to_string(),
            ])
            .unwrap();
        assert_eq!(balance.confirmed, 165000000000);
        assert_eq!(balance.unconfirmed, 0);
    }

    #[test]
    fn local_balance_surfaces_the_error_field() {
        let err = WocLocalInterface::new("http://localhost:5010")
            .parse_balance(&[
                r#"{"confirmed":0,"error":"bad address"}"#.to_string(),
                r#"{"unconfirmed":0,"error":""}"#.to_string(),
            ])
            .unwrap_err();
        assert!(format!("{err:?}").contains("bad address"));
    }

    #[test]
    fn public_balance_deserialises() {
        let balance = WocPublicInterface::new()
            .parse_balance(&[r#"{"confirmed":165000000000,"unconfirmed":7}"#.to_string()])
            .unwrap();
        assert_eq!(balance.confirmed, 165000000000);
        assert_eq!(balance.unconfirmed, 7);
    }

    /// Live checks against the `woc` Docker profile. Ignored by default --
    /// they need the stack up:
    ///
    ///     docker compose -f docker/docker-compose.yml \
    ///                    -f docker/woc/docker-compose.wbl.yml --profile woc up -d
    ///
    /// Then, with an address that has regtest coins:
    ///
    ///     WOC_TEST_ADDRESS=<address> cargo test woc_interface -- --ignored --nocapture
    mod live {
        use super::*;

        fn client() -> WocClient<WocLocalInterface> {
            let url = std::env::var("WOC_TEST_URL")
                .unwrap_or_else(|_| "http://localhost:5010".to_string());
            WocClient::new(WocLocalInterface::new(&url))
        }

        #[tokio::test]
        #[ignore = "requires the woc Docker profile to be running"]
        async fn status_reaches_local_woc() {
            client().status().await.expect("status failed");
        }

        #[tokio::test]
        #[ignore = "requires the woc Docker profile to be running"]
        async fn get_balance_returns_confirmed_and_unconfirmed() {
            let Ok(address) = std::env::var("WOC_TEST_ADDRESS") else {
                eprintln!("WOC_TEST_ADDRESS unset -- skipping");
                return;
            };
            let balance = client()
                .get_balance(&address)
                .await
                .expect("get_balance failed");
            println!("balance = {:?}", balance);
            assert!(balance.confirmed > 0, "expected a funded address");
        }

        #[tokio::test]
        #[ignore = "requires the woc Docker profile to be running"]
        async fn get_utxo_unwraps_the_result_array() {
            let Ok(address) = std::env::var("WOC_TEST_ADDRESS") else {
                eprintln!("WOC_TEST_ADDRESS unset -- skipping");
                return;
            };
            let utxo = client().get_utxo(&address).await.expect("get_utxo failed");
            println!("utxo count = {}", utxo.len());
            assert!(!utxo.is_empty(), "expected unspent outputs");
            assert!(utxo[0].value > 0);
            assert!(!utxo[0].tx_hash.is_empty());
        }

        #[tokio::test]
        #[ignore = "requires the woc Docker profile to be running"]
        async fn get_block_headers_serves_the_liveness_probe() {
            // service.rs uses this purely as a connectivity check.
            client()
                .get_block_headers()
                .await
                .expect("get_block_headers failed");
        }
    }
}
