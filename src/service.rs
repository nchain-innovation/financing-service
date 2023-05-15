use serde::Serialize;
use std::time::SystemTime;

use chrono::prelude::DateTime;
use chrono::Utc;

use chain_gang::messages::{OutPoint, Tx};

use crate::blockchain_interface::{blockchain_factory, BlockchainInterface};
use crate::client::Client;
use crate::config::Config;
use crate::util::tx_as_hexstr;

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
    blockchain_interface: Box<dyn BlockchainInterface + Send + Sync>,
    clients: Vec<Client>,
}

impl Service {
    /// Create a new Service from the provided config
    pub async fn new(config: &Config) -> Service {
        let mut clients: Vec<Client> = Vec::new();

        let blockchain_interface = blockchain_factory(config);
        for client_config in &config.client {
            let new_client = Client::new(client_config, blockchain_interface.get_network());
            clients.push(new_client);
        }

        let mut service = Service {
            blockchain_status: BlockchainConnectionStatus::Unknown,
            blockchain_update_time: None,
            blockchain_interface,
            clients,
        };
        service.update_balances().await;
        service
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

        let mut retval = format!(
            "{{\"blockchain_status\": \"{:?}\", \"blockchain_update_time\": \"{}\", \"clients\":[",
            self.blockchain_status, update_time
        );

        let len = self.clients.len();
        for (i, c) in self.clients.iter().enumerate() {
            retval += &c.get_balance();
            // Add comma to all but the last entry
            if i + 1 < len {
                retval += ",";
            }
        }
        retval += "]}";
        retval
    }

    /// Update client balances
    pub async fn update_balances(&mut self) {
        for client in &mut self.clients {
            self.blockchain_status = match client.update_balance(&*self.blockchain_interface).await
            {
                Ok(_) => BlockchainConnectionStatus::Connected,
                Err(_) => BlockchainConnectionStatus::Failed,
            };
            self.blockchain_update_time = Some(SystemTime::now());
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

                match self.blockchain_interface.broadcast_tx(&tx_as_str).await {
                    Ok(result) if result.status() == 200u16 => {
                        // append to the list
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
                            // provide the outpoints so far

                            return format!("{{\"status\": \"Failure\", \"description\": \"Failed to broadcast funding transaction.\",\"outpoints\": {outpoints_as_str}}}");
                        }
                    }
                }
            }
            //provide all the outpoints
            let outpoints_as_str = self.outpoints_to_string(&outpoints);
            format!("{{\"status\": \"Success\", \"outpoints\": {outpoints_as_str}}}")
        } else {
            // Create one tx
            let b_tx: Tx = client
                .create_funding_tx(satoshi, no_of_outpoints, locking_script)
                .unwrap();
            // broadcast tx
            let tx_as_str = tx_as_hexstr(&b_tx);
            dbg!(&tx_as_str);
            match self.blockchain_interface.broadcast_tx(&tx_as_str).await {
                Ok(result) if result.status() == 200u16 => {
                    dbg!(&result);
                    println!("result.status = {}", result.status() );
                    let result_text = result.text().await.unwrap();

                    println!("result.text() = {}", result_text);
                    let outpoints = self.get_outpoints(&b_tx.hash().encode(), no_of_outpoints);
                    format!("{{\"status\": \"Success\", \"outpoints\": {outpoints}}}")
                }
                _ => {
                    "{{\"status\": \"Failure\", \"description\": \"Failed to broadcast funding transaction.\"}}".to_string()
                }
            }
        }
    }
}
