use crate::config::Config;
use serde::{Deserialize, Serialize};
use sv::network::Network;

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
        Network::Mainnet => bitcoin::Network::Bitcoin,
        Network::Testnet => bitcoin::Network::Testnet,
        Network::STN => bitcoin::Network::Signet,
    }
}

/// Structure for json serialisation for broadcast_tx
#[derive(Debug, Serialize)]
pub struct BroadcastTxType {
    pub txhex: String,
}

/// Represents an interface to the blockchain
#[allow(dead_code)]
#[derive(Debug)]
pub struct BlockchainInterface {
    /// the interface type associated with this interface - currently only supports WoC - WhatsOnChain
    interface_type: String,
    /// the network associated with this interface
    network_type: Network,
}

impl BlockchainInterface {
    pub fn new(config: &Config) -> Self {
        BlockchainInterface {
            interface_type: config.blockchain_interface.interface_type.clone(),
            network_type: config.get_network().unwrap(),
        }
    }

    /// Return the current network as a string
    fn get_network_str(&self) -> &'static str {
        match self.network_type {
            Network::Mainnet => "main",
            Network::Testnet => "test",
            Network::STN => "stn",
        }
    }

    /// Return the network associated with this interface
    pub fn get_network(&self) -> Network {
        self.network_type
    }

    /// Get balance associated with address
    pub async fn get_balance(
        &self,
        address: &str,
    ) -> Result<WocBalance, Box<dyn std::error::Error>> {
        let network = self.get_network_str();
        let url =
            format!("https://api.whatsonchain.com/v1/bsv/{network}/address/{address}/balance");
        let response = reqwest::get(url).await?;
        let data = response.json::<WocBalance>().await?;
        dbg!(&data);
        Ok(data)
    }

    /// Get UXTO associated with address
    pub async fn get_utxo(&self, address: &str) -> Result<WocUtxo, Box<dyn std::error::Error>> {
        let network = self.get_network_str();

        let url =
            format!("https://api.whatsonchain.com/v1/bsv/{network}/address/{address}/unspent");
        let response = reqwest::get(url).await?;
        let data = response.json::<WocUtxo>().await?;

        dbg!(&data);
        Ok(data)
    }

    /// Broadcast Tx
    pub async fn broadcast_tx(&self, tx: &str) -> Result<reqwest::Response, reqwest::Error> {
        println!("broadcast_tx");
        let network = self.get_network_str();

        let url = format!("https://api.whatsonchain.com/v1/bsv/{network}/tx/raw");

        let data_for_broadcast = BroadcastTxType {
            txhex: tx.to_string(),
        };
        let data = serde_json::to_string(&data_for_broadcast).unwrap();
        // = format!("{{\"txhex\" : \"{tx}\"}}");
        dbg!(&data);
        let client = reqwest::Client::new();
        client.post(url).json(&data).send().await
    }
}

// address for test mwxrVFsJps3sxz5A38Mbrze8kPKq7D5NxF

#[cfg(test)]
mod tests {
    use super::*;
    //use tokio_test;

    fn get_blockchain_interface() -> BlockchainInterface {
        BlockchainInterface {
            interface_type: "WoC".to_string(),
            network_type: Network::Testnet,
        }
    }

    #[tokio::test]
    async fn test_get_balance() {
        let bci = get_blockchain_interface();

        let result = bci.get_balance("mwxrVFsJps3sxz5A38Mbrze8kPKq7D5NxF").await;
        dbg!(&result);
        assert!(&result.is_ok());
    }

    #[tokio::test]
    async fn test_get_utxo() {
        let bci = get_blockchain_interface();

        let result = bci.get_utxo("mwxrVFsJps3sxz5A38Mbrze8kPKq7D5NxF").await;
        dbg!(&result);
        assert!(&result.is_ok());
    }
}
