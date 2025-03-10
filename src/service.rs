use serde::Serialize;
use std::time::SystemTime;

use chain_gang::{
    interface::{Balance, BlockchainInterface},
    messages::{OutPoint, Tx},
    util::Hash256,
};
use chrono::prelude::DateTime;
use chrono::Utc;

use crate::{
    blockchain_factory::blockchain_factory,
    client::{Client, FundRequest},
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

#[derive(Clone, Default)]
pub struct FundingResponse {
    pub outpoints: Vec<OutPoint>,
    pub txs: Vec<Tx>,
}

impl FundingResponse {
    /// Given outpoints return them as a string
    fn outpoints_to_json(&self) -> String {
        let mut retval: String = "[".to_string();

        for (i, op) in self.outpoints.iter().enumerate() {
            retval += format!(
                "{{\"hash\": \"{}\", \"index\": {}}}",
                op.hash.encode(),
                op.index
            )
            .as_str();
            if i + 1 != self.outpoints.len() {
                retval += ",";
            }
        }
        retval += "]";
        retval
    }

    fn txs_to_json(&self) -> String {
        //                 let tx_as_str = tx_as_hexstr(&a_tx);
        let mut retval: String = "[".to_string();

        for (i, tx) in self.txs.iter().enumerate() {
            retval += format!("{{\"tx\": \"{}\"}}", tx_as_hexstr(tx)).as_str();
            if i + 1 != self.txs.len() {
                retval += ",";
            }
        }
        retval += "]";
        retval
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"outpoints\":  {}, \"txs\": {}}}",
            self.outpoints_to_json(),
            self.txs_to_json()
        )
    }
}

/// Service data
//#[derive(Debug)]
pub struct Service {
    blockchain_status: BlockchainConnectionStatus,
    blockchain_update_time: Option<SystemTime>,
    blockchain_interface: Box<dyn BlockchainInterface>,
    clients: Vec<Client>,
    dynamic_config: DynamicConfig,
}

impl Service {
    /// Create a new Service from the provided config
    pub async fn new(config: &Config) -> Service {
        let mut clients: Vec<Client> = Vec::new();
        let blockchain_interface = blockchain_factory(config);

        // Check we can connect to blockchain
        blockchain_interface
            .status()
            .await
            .expect("Unable to connect to blockchain, ensure that the service is running.");

        if let Some(clients_config) = &config.client {
            for client_config in clients_config {
                let new_client = Client::new(client_config);
                clients.push(new_client);
            }
        }

        // Add the dynamic clients
        let dynamic_config = DynamicConfig::new(config);
        for client_config in &dynamic_config.contents.clients {
            let new_client = Client::new(client_config);
            clients.push(new_client);
        }

        let mut service = Service {
            blockchain_status: BlockchainConnectionStatus::Unknown,
            blockchain_update_time: None,
            blockchain_interface,
            clients,
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
        let new_client = Client::new(&client_config);
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
                log::warn!("get_block_headers - failed {:?}", e);
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
    pub fn get_balance(&self, client_id: &str) -> Option<Balance> {
        let client = self.clients.iter().find(|x| x.client_id == client_id)?;
        Some(client.get_balance())
    }

    pub fn get_address(&self, client_id: &str) -> Option<String> {
        let client = self.clients.iter().find(|x| x.client_id == client_id)?;
        Some(client.get_address())
    }

    /// Return true if the client has sufficient balance for this transaction
    pub fn has_sufficent_balance(&self, fund_request: &FundRequest) -> Option<bool> {
        let client = self
            .clients
            .iter()
            .find(|x| x.client_id == fund_request.client_id)?;
        client.has_sufficent_balance(fund_request)
    }

    /// Given txid and no_of_outpoints return the outpoints as JSON string
    fn get_outpoints(&self, hash: Hash256, no_of_outpoints: u32) -> Vec<OutPoint> {
        (1..no_of_outpoints + 1)
            .map(|index| OutPoint { hash, index })
            .collect()
    }

    /// Create funding outpoints based on the provided arguments
    pub async fn create_funding_outpoints(
        &mut self,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, String> {
        let client: &mut Client = self
            .clients
            .iter_mut()
            .find(|x| x.client_id == fund_request.client_id)
            .unwrap();

        let mut response = FundingResponse::default();
        // Check balance
        if fund_request.no_of_outpoints > 1 && fund_request.multiple_tx {
            // Create multiple tx
            response.txs = client.create_multiple_funding_txs(fund_request);

            // broadcast multiple txs
            for a_tx in &response.txs {
                // broadcast tx
                let tx_as_str = tx_as_hexstr(a_tx);
                log::info!("tx_as_str = {}", &tx_as_str);

                match self.blockchain_interface.broadcast_tx(a_tx).await {
                    Ok(_hash) => {
                        // Append to the list
                        // Note the provided hash is a str whereas OutPoint wants a Hash256
                        response.outpoints.push(OutPoint {
                            hash: a_tx.hash(),
                            index: 1,
                        });
                    }
                    _ => {
                        log::info!("Failed to broadcast funding transaction");
                        return Err(
                            "{\"description\": \"Failed to broadcast funding transaction.\"}"
                                .to_string(),
                        );
                    }
                }
            }
            // Provide all the outpoints
            Ok(response)
        } else {
            // Create one tx
            let b_tx: Tx = client.create_funding_tx(fund_request).unwrap();
            // broadcast tx
            //let tx_as_str = tx_as_hexstr(&b_tx);
            response.txs.push(b_tx.clone());

            match self.blockchain_interface.broadcast_tx(&b_tx).await {
                Ok(_hash) => {
                    let hash = b_tx.hash();
                    response.outpoints = self.get_outpoints(hash, fund_request.no_of_outpoints);
                    Ok(response)
                }
                _ => {
                    log::info!("Failed to broadcast funding transaction");
                    Err(
                        "{\"description\": \"Failed to broadcast funding transaction.\"}"
                            .to_string(),
                    )
                }
            }
        }
    }
}
