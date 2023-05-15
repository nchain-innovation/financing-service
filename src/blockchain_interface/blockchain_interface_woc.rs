use async_trait::async_trait;

use crate::config::Config;
use chain_gang::network::Network;
use serde::Serialize;

use super::blockchain_if::{BlockchainInterface, WocBalance, WocUtxo};

/// Structure for json serialisation for broadcast_tx
#[derive(Debug, Serialize)]
pub struct BroadcastTxType {
    pub txhex: String,
}

/// Represents an interface to the blockchain
#[allow(dead_code)]
#[derive(Debug)]
pub struct BlockchainInterfaceWoc {
    interface_type: String,
    /// the network associated with this interface
    network_type: Network,
}

impl BlockchainInterfaceWoc {
    pub fn new(config: &Config) -> Self {
        BlockchainInterfaceWoc {
            interface_type: config.blockchain_interface.interface_type.clone(),
            network_type: config.get_network().unwrap(),
        }
    }

    /// Return the current network as a string
    fn get_network_str(&self) -> &'static str {
        match self.network_type {
            Network::BSV_Mainnet => "main",
            Network::BSV_Testnet => "test",
            Network::BSV_STN => "stn",
            _ => "unknown",
        }
    }
}

#[async_trait]
impl BlockchainInterface for BlockchainInterfaceWoc {
    /// Return the network associated with this interface
    fn get_network(&self) -> Network {
        self.network_type
    }

    /// Get balance associated with address
    async fn get_balance(&self, address: &str) -> Result<WocBalance, Box<dyn std::error::Error>> {
        let network = self.get_network_str();
        let url =
            format!("https://api.whatsonchain.com/v1/bsv/{network}/address/{address}/balance");
        let response = reqwest::get(url).await?;
        let data = response.json::<WocBalance>().await?;
        dbg!(&address);
        dbg!(&data);
        Ok(data)
    }

    /// Get UXTO associated with address
    async fn get_utxo(&self, address: &str) -> Result<WocUtxo, Box<dyn std::error::Error>> {
        let network = self.get_network_str();

        let url =
            format!("https://api.whatsonchain.com/v1/bsv/{network}/address/{address}/unspent");
        let response = reqwest::get(url).await?;
        let data = response.json::<WocUtxo>().await?;
        dbg!(&address);
        dbg!(&data);
        Ok(data)
    }

    /// Broadcast Tx
    async fn broadcast_tx(&self, tx: &str) -> Result<reqwest::Response, reqwest::Error> {
        println!("broadcast_tx");
        let network = self.get_network_str();

        let url = format!("https://api.whatsonchain.com/v1/bsv/{network}/tx/raw");
        dbg!(&url);
        let data_for_broadcast = BroadcastTxType {
            txhex: tx.to_string(),
        };
        let data = serde_json::to_string(&data_for_broadcast).unwrap();
        dbg!(&data);
        let client = reqwest::Client::new();
        client.post(url).json(&data).send().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_blockchain_interface() -> BlockchainInterfaceWoc {
        BlockchainInterfaceWoc {
            interface_type: "WoC".to_string(),
            network_type: Network::Testnet,
        }
    }
    // address for test mwxrVFsJps3sxz5A38Mbrze8kPKq7D5NxF

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
