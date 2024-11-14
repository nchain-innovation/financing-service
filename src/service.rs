use serde::Serialize;
use std::time::SystemTime;

use chain_gang::{
    interface::BlockchainInterface,
    messages::{OutPoint, Tx},
    network::Network,
};
use chrono::prelude::DateTime;
use chrono::Utc;

use crate::{
    blockchain_factory::blockchain_factory,
    client::Client,
    config::{ClientConfig, Config},
    dynamic_config::DynamicConfig,
    util::tx_as_hexstr,
};

/// Blockchain Connection Status
#[derive(Debug, Serialize, Clone, Copy)]
pub enum BlockchainConnectionStatus {
    /// Unknown - Starting state of the service
    Unknown,
    /// Failed - The service has failed to connect to the blockchain
    Failed,
    /// Connected - The service has connected to the blockchain
    Connected,
}

/// Service data
//#[derive(Debug)]
pub struct Service {
    blockchain_status: BlockchainConnectionStatus,
    blockchain_update_time: Option<SystemTime>,
    blockchain_interface: Box<dyn BlockchainInterface>,
    clients: Vec<Client>,
    network: Network,
    dynamic_config: DynamicConfig,
}

impl Service {
    /// Create a new Service from the provided config
    pub async fn new(config: &Config) -> Service {
        let mut clients: Vec<Client> = Vec::new();
        let network = config.get_network().unwrap();
        let blockchain_interface = blockchain_factory(config);

        // Check we can connect to blockchain
        blockchain_interface
            .status()
            .await
            .expect("Unable to connect to blockchain");

        if let Some(clients_config) = &config.client {
            for client_config in clients_config {
                let new_client = Client::new(client_config, network);
                clients.push(new_client);
            }
        }

        // Add the dynamic clients
        let dynamic_config = DynamicConfig::new(config);
        for client_config in &dynamic_config.contents.clients {
            let new_client = Client::new(client_config, network);
            clients.push(new_client);
        }

        let mut service = Service {
            blockchain_status: BlockchainConnectionStatus::Unknown,
            blockchain_update_time: None,
            blockchain_interface,
            clients,
            network,
            dynamic_config,
        };
        service.update_balances().await;
        service
    }

    pub fn add_client(&mut self, client_id: &str, wif: &str) {
        let client_config = ClientConfig {
            client_id: client_id.to_string(),
            wif_key: wif.to_string(),
        };
        let new_client = Client::new(&client_config, self.network);
        self.clients.push(new_client);
        // save dynamic info
        self.dynamic_config.add(&client_config);
    }

    pub fn delete_client(&mut self, client_id: &str) {
        // let new_client = Client::new(&client_config, self.network);
        if let Some(index) = self.clients.iter().position(|c| c.client_id == client_id) {
            self.clients.remove(index);
        }
        // save dynamic info
        self.dynamic_config.remove(client_id);
    }

    /// Return the Service status as a JSON string
    pub fn get_status(&self) -> String {
        let update_time = match self.blockchain_update_time {
            Some(time) => {
                let datetime = DateTime::<Utc>::from(time);
                datetime.format("%Y-%m-%d %H:%M:%S").to_string()
            }
            None => "None".to_string(),
        };
        let version = env!("CARGO_PKG_VERSION");
        format!(
            "{{\"version\": \"{}\", \"blockchain_status\": \"{:?}\", \"blockchain_update_time\": \"{}\"}}",
            version, self.blockchain_status, update_time
        )
    }

    async fn get_block_headers(&mut self) {
        self.blockchain_status = match self.blockchain_interface.get_block_headers().await {
            Ok(_) => BlockchainConnectionStatus::Connected,
            Err(e) => {
                log::warn!("update_balance - failed {:?}", e);
                BlockchainConnectionStatus::Failed
            }
        };
        self.blockchain_update_time = Some(SystemTime::now());
    }

    /// Update client balances
    pub async fn update_balances(&mut self) {
        if self.clients.is_empty() {
            // Request latest block header - to determine the blockchain connectivity status
            self.get_block_headers().await;
        } else {
            // Get client balances
            for client in &mut self.clients {
                self.blockchain_status =
                    match client.update_balance(&*self.blockchain_interface).await {
                        Ok(_) => BlockchainConnectionStatus::Connected,
                        Err(e) => {
                            log::warn!("update_balance - failed {:?}", e);
                            BlockchainConnectionStatus::Failed
                        }
                    };
                self.blockchain_update_time = Some(SystemTime::now());
            }
        }
    }

    /// Given a client_id return true if it is valid
    pub fn is_client_id_valid(&self, client_id: &str) -> bool {
        self.clients.iter().any(|x| x.client_id == client_id)
    }

    /// Given a client_id return the associated balance as JSON string
    pub fn get_balance(&self, client_id: &str) -> Option<String> {
        let client = self.clients.iter().find(|x| x.client_id == client_id)?;
        Some(client.get_balance())
    }

    pub fn get_address(&self, client_id: &str) -> Option<String> {
        let client = self.clients.iter().find(|x| x.client_id == client_id)?;
        Some(client.get_address())
    }

    /// Return true if the client has sufficient balance for this transaction
    pub fn has_sufficent_balance(
        &self,
        client_id: &str,
        satoshi: u64,
        no_of_outpoints: u32,
        multiple_tx: bool,
        locking_script: &[u8],
    ) -> Option<bool> {
        let client = self.clients.iter().find(|x| x.client_id == client_id)?;
        client.has_sufficent_balance(satoshi, no_of_outpoints, multiple_tx, locking_script)
    }

    /// Given txid and no_of_outpoints return the outpoints as JSON string
    fn get_outpoints(&self, hash: &str, no_of_outpoints: u32) -> String {
        let mut retval: String = "[".to_string();

        for i in 1..no_of_outpoints + 1 {
            retval += format!("{{\"hash\": \"{hash}\", \"index\": {i}}}").as_str();
            if i != no_of_outpoints {
                retval += ",";
            }
        }
        retval += "]";
        retval
    }

    /// Given outpoints return them as a string
    /// Given outpoints return them as a string
    fn outpoints_to_string(&self, outpoints: &[OutPoint]) -> String {
        let mut retval: String = "[".to_string();

        for (i, op) in outpoints.iter().enumerate() {
            retval += format!(
                "{{\"hash\": \"{}\", \"index\": {}}}",
                op.hash.encode(),
                op.index
            )
            .as_str();
            if i != outpoints.len() {
                retval += ",";
            }
        }
        retval += "]";
        retval
    }

    /// Create funding outpoints based on the provided arguments
    pub async fn create_funding_outpoints(
        &mut self,
        client_id: &str,
        satoshi: u64,
        no_of_outpoints: u32,
        multiple_tx: bool,
        locking_script: &[u8],
    ) -> String {
        let client: &mut Client = self
            .clients
            .iter_mut()
            .find(|x| x.client_id == client_id)
            .unwrap();

        // Check balance
        if no_of_outpoints > 1 && multiple_tx {
            // Create multiple tx
            let txs: Vec<Tx> =
                client.create_multiple_funding_txs(satoshi, no_of_outpoints, locking_script);
            let mut outpoints: Vec<OutPoint> = Vec::new();
            // broadcast multiple txs
            for a_tx in txs {
                // broadcast tx
                let tx_as_str = tx_as_hexstr(&a_tx);
                log::info!("tx_as_str = {}", &tx_as_str);

                match self.blockchain_interface.broadcast_tx(&a_tx).await {
                    Ok(_hash) => {
                        // Append to the list
                        // Note the provided hash is a str whereas OutPoint wants a Hash256
                        outpoints.push(OutPoint {
                            hash: a_tx.hash(),
                            index: 1,
                        });
                    }
                    _ => {
                        if outpoints.is_empty() {
                            return "{{\"status\": \"Failure\", \"description\": \"Failed to broadcast funding transaction.\"}}".to_string();
                        } else {
                            let outpoints_as_str = self.outpoints_to_string(&outpoints);
                            // Provide the outpoints so far
                            return format!("{{\"status\": \"Failure\", \"description\": \"Failed to broadcast funding transaction.\",\"outpoints\": {outpoints_as_str}}}");
                        }
                    }
                }
            }
            // Provide all the outpoints
            let outpoints_as_str = self.outpoints_to_string(&outpoints);
            format!("{{\"status\": \"Success\", \"outpoints\": {outpoints_as_str}}}")
        } else {
            // Create one tx
            let b_tx: Tx = client
                .create_funding_tx(satoshi, no_of_outpoints, locking_script)
                .unwrap();
            // broadcast tx
            let tx_as_str = tx_as_hexstr(&b_tx);
            log::info!("tx_as_str = {}", &tx_as_str);
            match self.blockchain_interface.broadcast_tx(&b_tx).await {
                Ok(hash)//if result.status() == 200u16 => {
                    => {
                    let outpoints = self.get_outpoints(&hash, no_of_outpoints);
                    format!("{{\"status\": \"Success\", \"outpoints\": {outpoints}, \"tx\": \"{tx_as_str}\"}}")
                },
                _ => {
                    "{{\"status\": \"Failure\", \"description\": \"Failed to broadcast funding transaction.\"}}".to_string()
                },
            }
        }
    }
}
