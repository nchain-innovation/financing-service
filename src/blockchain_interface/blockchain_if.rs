use async_trait::async_trait;

use chain_gang::network::Network;
use serde::Deserialize;

#[allow(unused_must_use)]

/// Balance returned from WoC
#[derive(Debug, Deserialize, Clone, Copy)]
pub struct WocBalance {
    pub confirmed: u64,
    pub unconfirmed: u64,
}

impl WocBalance {
    pub fn new() -> Self {
        WocBalance {
            confirmed: 0,
            unconfirmed: 0,
        }
    }
}

/// Type to represent UTXO Entry
#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct WocUtxoEntry {
    pub height: u32,
    pub tx_pos: u32,
    pub tx_hash: String,
    pub value: u64,
}
/// Type to represent UTXO set
pub type WocUtxo = Vec<WocUtxoEntry>;

/// Convert network to bitcoin network type
pub fn as_bitcoin_network(network: &Network) -> bitcoin::Network {
    match network {
        Network::BSV_Mainnet => bitcoin::Network::Bitcoin,
        Network::BSV_Testnet => bitcoin::Network::Testnet,
        Network::BSV_STN => bitcoin::Network::Signet,
        _ => panic!("unknown network {}", &network),
    }
}

/// BlockchainInterface trait
#[async_trait]
pub trait BlockchainInterface: Send + Sync {
    /// Return the network associated with this interface
    fn get_network(&self) -> Network;

    /// Get balance associated with address
    async fn get_balance(&self, address: &str) -> Result<WocBalance, Box<dyn std::error::Error>>;

    /// Get UXTO associated with address
    async fn get_utxo(&self, address: &str) -> Result<WocUtxo, Box<dyn std::error::Error>>;

    /// Broadcast Tx
    async fn broadcast_tx(&self, tx: &str) -> Result<reqwest::Response, reqwest::Error>;
}
