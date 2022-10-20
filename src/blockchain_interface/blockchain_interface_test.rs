use async_trait::async_trait;
use std::collections::HashMap;

use crate::config::Config;
use serde::Serialize;
use sv::network::Network;

use crate::blockchain_interface::{BlockchainInterface, WocBalance, WocUtxo};

/// Structure for json serialisation for broadcast_tx
#[derive(Debug, Serialize)]
pub struct BroadcastTxType {
    pub txhex: String,
}

/// Represents an interface to the blockchain, used for testing
#[allow(dead_code)]
#[derive(Debug)]
pub struct BlockchainInterfaceTest {
    interface_type: String,
    /// the network associated with this interface
    network_type: Network,
    utxo: HashMap<String, WocUtxo>,
    height: u32,
    broadcast: Vec<String>,
}

#[allow(dead_code)]
impl BlockchainInterfaceTest {
    pub fn new(config: &Config) -> Self {
        BlockchainInterfaceTest {
            interface_type: config.blockchain_interface.interface_type.clone(),
            network_type: config.get_network().unwrap(),
            utxo: HashMap::new(),
            height: 0,
            broadcast: Vec::new(),
        }
    }

    pub fn set_utxo(&mut self, address: &str, utxo: &WocUtxo) {
        self.utxo.insert(address.to_string(), utxo.to_vec());
    }

    pub fn set_height(&mut self, height: u32) {
        self.height = height;
    }
}

#[async_trait]
impl BlockchainInterface for BlockchainInterfaceTest {
    /// Return the network associated with this interface
    fn get_network(&self) -> Network {
        self.network_type
    }

    /// Get balance associated with address
    async fn get_balance(&self, address: &str) -> Result<WocBalance, Box<dyn std::error::Error>> {
        let utxo: WocUtxo = self.get_utxo(address).await?;
        let confirmation_height = self.height - 6;

        let confirmed: u64 = utxo
            .iter()
            .filter(|x| x.height <= confirmation_height)
            .map(|x| x.value)
            .sum();

        let unconfirmed: u64 = utxo
            .iter()
            .filter(|x| x.height > confirmation_height)
            .map(|x| x.value)
            .sum();

        let balance: WocBalance = WocBalance {
            confirmed,
            unconfirmed,
        };
        Ok(balance)
    }

    /// Get UXTO associated with address
    async fn get_utxo(&self, address: &str) -> Result<WocUtxo, Box<dyn std::error::Error>> {
        match self.utxo.get(address) {
            Some(value) => Ok(value.to_vec()),
            None => Ok(Vec::new()),
        }
    }

    /// Broadcast Tx
    async fn broadcast_tx(&mut self, tx: &str) -> Result<reqwest::Response, reqwest::Error> {
        println!("broadcast_tx");

        // Record tx
        self.broadcast.push(tx.to_string());
        // Spoof request to provide an async response
        let url = format!("https://www.google.com");
        reqwest::get(url).await
    }
}
