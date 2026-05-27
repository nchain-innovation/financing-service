use std::{sync::Arc, time::SystemTime};

use chain_gang::{
    interface::{Balance, BlockchainInterface},
    messages::{OutPoint, Tx},
};
use chrono::prelude::DateTime;
use chrono::Utc;
use tokio::sync::RwLock;

use crate::{
    blockchain_factory::blockchain_factory,
    client::{Client, FundRequest},
    config::{ClientConfig, Config},
    dynamic_config::DynamicConfig,
    responses::{
        BlockchainConnectionStatus, FundingResponseJson, OutpointResponse, StatusResponse,
        TxResponse,
    },
    util::tx_as_hexstr,
};

#[derive(Clone, Default)]
pub struct FundingResponse {
    pub outpoints: Vec<OutPoint>,
    pub txs: Vec<Tx>,
}

impl FundingResponse {
    pub fn to_response(&self) -> Result<FundingResponseJson, String> {
        let outpoints = self
            .outpoints
            .iter()
            .map(|op| OutpointResponse {
                hash: op.hash.encode(),
                index: op.index,
            })
            .collect();
        let mut txs = Vec::with_capacity(self.txs.len());
        for tx in &self.txs {
            txs.push(TxResponse {
                tx: tx_as_hexstr(tx)?,
            });
        }
        Ok(FundingResponseJson { outpoints, txs })
    }
}

#[derive(Clone, Default)]
pub struct PreparedFunding {
    pub txs: Vec<Tx>,
    pub no_of_outpoints: u32,
    pub multiple_tx: bool,
}

/// Service data
//#[derive(Debug)]
pub struct Service {
    blockchain_status: BlockchainConnectionStatus,
    blockchain_update_time: Option<SystemTime>,
    blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
    clients: Vec<Client>,
    dynamic_config: DynamicConfig,
    admin_api_key: Option<String>,
}

impl Service {
    async fn build(
        config: &Config,
        blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
    ) -> Service {
        let mut clients: Vec<Client> = Vec::new();

        if let Some(clients_config) = &config.client {
            for client_config in clients_config {
                clients.push(Client::new(client_config));
            }
        }

        let dynamic_config = DynamicConfig::new(config);
        for client_config in &dynamic_config.contents.clients {
            clients.push(Client::new(client_config));
        }

        Service {
            blockchain_status: BlockchainConnectionStatus::Unknown,
            blockchain_update_time: None,
            blockchain_interface,
            clients,
            dynamic_config,
            admin_api_key: config
                .web_interface
                .admin_api_key
                .clone()
                .filter(|key| !key.is_empty()),
        }
    }

    /// Create a new Service from the provided config
    pub async fn new(config: &Config) -> Service {
        let blockchain_interface = blockchain_factory(config);

        blockchain_interface
            .status()
            .await
            .expect("Unable to connect to blockchain, ensure that the service is running.");

        let mut service = Self::build(config, blockchain_interface).await;
        service.update_balances().await;
        service
    }

    #[cfg(test)]
    pub async fn new_for_test(
        config: &Config,
        blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
    ) -> Service {
        blockchain_interface
            .status()
            .await
            .expect("Unable to connect to test blockchain.");

        let mut service = Self::build(config, blockchain_interface).await;
        service.update_balances().await;
        service
    }

    pub fn add_client(
        &mut self,
        client_id: &str,
        wif: &str,
        api_key: Option<&str>,
    ) -> Result<(), String> {
        let client_config = ClientConfig {
            client_id: client_id.to_string(),
            wif_key: wif.to_string(),
            api_key: api_key.map(str::to_string),
        };
        let new_client = Client::try_new(&client_config)?;
        self.clients.push(new_client);
        if let Err(e) = self.dynamic_config.add(&client_config) {
            self.clients.pop();
            return Err(e);
        }
        Ok(())
    }

    pub fn delete_client(&mut self, client_id: &str) -> Result<(), String> {
        if let Some(index) = self.clients.iter().position(|c| c.client_id == client_id) {
            self.clients.remove(index);
        }
        self.dynamic_config.remove(client_id)
    }

    /// Return the Service status
    pub fn get_status(&self) -> StatusResponse {
        let update_time = match self.blockchain_update_time {
            Some(time) => {
                let datetime = DateTime::<Utc>::from(time);
                datetime.format("%Y-%m-%d %H:%M:%S").to_string()
            }
            None => "None".to_string(),
        };
        StatusResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            blockchain_status: self.blockchain_status,
            blockchain_update_time: update_time,
        }
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

    /// Update client balances while holding a mutable reference (startup only).
    pub async fn update_balances(&mut self) {
        if self.clients.is_empty() {
            self.get_block_headers().await;
        } else {
            for client in &mut self.clients {
                self.blockchain_status = match client
                    .update_balance(self.blockchain_interface.as_ref())
                    .await
                {
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

    /// Refresh balances without holding the service lock during blockchain I/O.
    pub async fn refresh_balances(service: &RwLock<Service>) {
        let (blockchain, addresses) = {
            let service = service.read().await;
            (
                Arc::clone(&service.blockchain_interface),
                service
                    .clients
                    .iter()
                    .map(|client| client.get_address())
                    .collect::<Vec<_>>(),
            )
        };

        if addresses.is_empty() {
            let mut service = service.write().await;
            service.get_block_headers().await;
            return;
        }

        let mut chain_updates = Vec::with_capacity(addresses.len());
        for address in addresses {
            let balance = blockchain.get_balance(&address).await;
            let utxo = blockchain.get_utxo(&address).await;
            chain_updates.push((balance, utxo));
        }

        let mut service = service.write().await;
        let mut status = BlockchainConnectionStatus::Connected;
        for (client, (balance_result, utxo_result)) in service.clients.iter_mut().zip(chain_updates)
        {
            match (balance_result, utxo_result) {
                (Ok(balance), Ok(utxo)) => client.apply_chain_state(balance, utxo),
                (Err(e), _) | (_, Err(e)) => {
                    log::warn!("update_balance - failed {:?}", e);
                    status = BlockchainConnectionStatus::Failed;
                }
            }
        }
        service.blockchain_status = status;
        service.blockchain_update_time = Some(SystemTime::now());
    }

    /// Given a client_id return true if it is valid
    pub fn is_client_id_valid(&self, client_id: &str) -> bool {
        self.clients.iter().any(|x| x.client_id == client_id)
    }

    pub fn client_auth_required(&self, client_id: &str) -> bool {
        self.clients
            .iter()
            .find(|client| client.client_id == client_id)
            .and_then(|client| client.api_key())
            .is_some()
    }

    pub fn verify_client_api_key(&self, client_id: &str, provided: &str) -> bool {
        match self
            .clients
            .iter()
            .find(|client| client.client_id == client_id)
        {
            Some(client) => match client.api_key() {
                Some(expected) => crate::auth::constant_time_eq(provided, expected),
                None => true,
            },
            None => false,
        }
    }

    pub fn clients_without_api_key(&self) -> Vec<&str> {
        self.clients
            .iter()
            .filter(|client| client.api_key().is_none())
            .map(|client| client.client_id.as_str())
            .collect()
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn blockchain_interface(&self) -> Arc<dyn BlockchainInterface + Send + Sync> {
        Arc::clone(&self.blockchain_interface)
    }

    pub fn admin_auth_required(&self) -> bool {
        self.admin_api_key.is_some()
    }

    pub fn verify_admin_api_key(&self, provided: &str) -> bool {
        match self.admin_api_key.as_deref() {
            Some(expected) => crate::auth::constant_time_eq(provided, expected),
            None => true,
        }
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

    /// Return a funding balance error for the client, if any.
    pub fn funding_balance_error(&self, fund_request: &FundRequest) -> Option<String> {
        let client = self
            .clients
            .iter()
            .find(|x| x.client_id == fund_request.client_id)?;
        client.funding_balance_error(fund_request)
    }

    /// Build and sign funding transactions, updating local UTXO state.
    pub fn prepare_funding_outpoints(
        &mut self,
        fund_request: &FundRequest,
    ) -> Result<PreparedFunding, String> {
        let client = self
            .clients
            .iter_mut()
            .find(|x| x.client_id == fund_request.client_id)
            .ok_or_else(|| format!("Unknown client_id {}", fund_request.client_id))?;

        if fund_request.no_of_outpoints > 1 && fund_request.multiple_tx {
            let txs = client.create_multiple_funding_txs(fund_request)?;
            Ok(PreparedFunding {
                txs,
                no_of_outpoints: fund_request.no_of_outpoints,
                multiple_tx: true,
            })
        } else {
            let tx = client.create_funding_tx(fund_request)?;
            Ok(PreparedFunding {
                txs: vec![tx],
                no_of_outpoints: fund_request.no_of_outpoints,
                multiple_tx: false,
            })
        }
    }

    /// Broadcast prepared funding transactions without holding the service lock.
    pub async fn broadcast_prepared_funding(
        blockchain: Arc<dyn BlockchainInterface + Send + Sync>,
        prepared: PreparedFunding,
    ) -> Result<FundingResponse, String> {
        let mut response = FundingResponse::default();
        if prepared.multiple_tx {
            for tx in &prepared.txs {
                let tx_as_str = tx_as_hexstr(tx)?;
                log::info!("tx_as_str = {}", &tx_as_str);
                blockchain.broadcast_tx(tx).await.map_err(|e| {
                    log::warn!("Failed to broadcast funding transaction: {:?}", e);
                    "Failed to broadcast funding transaction.".to_string()
                })?;
                response.outpoints.push(OutPoint {
                    hash: tx.hash(),
                    index: 1,
                });
            }
            response.txs = prepared.txs;
            Ok(response)
        } else {
            let tx = prepared
                .txs
                .into_iter()
                .next()
                .ok_or_else(|| "No funding transaction prepared.".to_string())?;
            response.txs.push(tx.clone());
            blockchain.broadcast_tx(&tx).await.map_err(|e| {
                log::warn!("Failed to broadcast funding transaction: {:?}", e);
                "Failed to broadcast funding transaction.".to_string()
            })?;
            let hash = tx.hash();
            response.outpoints = (1..prepared.no_of_outpoints + 1)
                .map(|index| OutPoint { hash, index })
                .collect();
            Ok(response)
        }
    }
}
