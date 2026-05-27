use std::{sync::Arc, time::SystemTime};

use chain_gang::{
    interface::{Balance, BlockchainInterface, Utxo},
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
    fn build(
        config: &Config,
        blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
    ) -> Result<Service, String> {
        let mut clients: Vec<Client> = Vec::new();

        if let Some(clients_config) = &config.client {
            for client_config in clients_config {
                clients.push(Client::try_new(client_config)?);
            }
        }

        let dynamic_config = DynamicConfig::new(config);
        for client_config in &dynamic_config.contents.clients {
            clients.push(Client::try_new(client_config)?);
        }

        Ok(Service {
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
        })
    }

    /// Create a new Service from the provided config
    pub async fn new(config: &Config) -> Result<Service, String> {
        let blockchain_interface = blockchain_factory(config)?;

        blockchain_interface.status().await.map_err(|e| {
            format!("Unable to connect to blockchain, ensure that the service is running: {e:?}")
        })?;

        let mut service = Self::build(config, blockchain_interface)?;
        service.update_balances().await;
        Ok(service)
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

        let mut service = Self::build(config, blockchain_interface)
            .expect("Invalid client configuration in test setup");
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
            chain_updates.push(fetch_chain_state(blockchain.as_ref(), &address).await);
        }

        let mut service = service.write().await;
        let mut status = BlockchainConnectionStatus::Connected;
        for (client, chain_state) in service.clients.iter_mut().zip(chain_updates) {
            match chain_state {
                Ok((balance, utxo)) => client.apply_chain_state(balance, utxo),
                Err(e) => {
                    log::warn!("update_balance - failed {}", e);
                    status = BlockchainConnectionStatus::Failed;
                }
            }
        }
        service.blockchain_status = status;
        service.blockchain_update_time = Some(SystemTime::now());
    }

    /// Refresh one client's balance and UTXO set from the blockchain before funding.
    pub async fn refresh_client_chain_state(
        service: &RwLock<Service>,
        client_id: &str,
    ) -> Result<(), String> {
        let (blockchain, address) = {
            let service = service.read().await;
            if !service.is_client_id_valid(client_id) {
                return Err(format!("Unknown client_id {client_id}"));
            }
            (
                service.blockchain_interface(),
                service
                    .get_address(client_id)
                    .ok_or_else(|| format!("Unknown client_id {client_id}"))?,
            )
        };

        let chain_state = fetch_chain_state(blockchain.as_ref(), &address).await?;

        let mut service = service.write().await;
        let client = service
            .clients
            .iter_mut()
            .find(|client| client.client_id == client_id)
            .ok_or_else(|| format!("Unknown client_id {client_id}"))?;
        client.apply_chain_state(chain_state.0, chain_state.1);
        service.blockchain_status = BlockchainConnectionStatus::Connected;
        service.blockchain_update_time = Some(SystemTime::now());
        Ok(())
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

        let tx = client.create_funding_tx(fund_request)?;
        Ok(PreparedFunding {
            txs: vec![tx],
            no_of_outpoints: fund_request.no_of_outpoints,
        })
    }

    /// Fund a request with separate transactions, preparing and broadcasting one at a time.
    pub async fn fund_with_multiple_transactions(
        service: &RwLock<Service>,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, String> {
        {
            let service = service.read().await;
            if let Some(description) = service.funding_balance_error(fund_request) {
                return Err(description);
            }
        }

        let per_tx_request = FundRequest {
            client_id: fund_request.client_id.clone(),
            satoshi: fund_request.satoshi,
            no_of_outpoints: 1,
            multiple_tx: false,
            locking_script: fund_request.locking_script.clone(),
        };

        let mut combined = FundingResponse::default();
        let total = fund_request.no_of_outpoints;

        for tx_index in 0..total {
            let (blockchain, prepared) = {
                let mut svc = service.write().await;
                let blockchain = svc.blockchain_interface();
                let prepared = svc.prepare_funding_outpoints(&per_tx_request);
                (blockchain, prepared)
            };

            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(cause) => {
                    if tx_index == 0 {
                        return Err(cause);
                    }
                    resync_after_multiple_tx_failure(service, fund_request).await;
                    return Err(partial_broadcast_error(
                        tx_index + 1,
                        total,
                        &combined,
                        cause,
                    ));
                }
            };

            match Self::broadcast_prepared_funding(blockchain, prepared).await {
                Ok(partial) => {
                    combined.outpoints.extend(partial.outpoints);
                    combined.txs.extend(partial.txs);
                }
                Err(cause) => {
                    if tx_index == 0 {
                        resync_after_multiple_tx_failure(service, fund_request).await;
                        return Err(cause);
                    }
                    resync_after_multiple_tx_failure(service, fund_request).await;
                    return Err(partial_broadcast_error(
                        tx_index + 1,
                        total,
                        &combined,
                        cause,
                    ));
                }
            }
        }

        Ok(combined)
    }

    /// Broadcast prepared funding transactions without holding the service lock.
    pub async fn broadcast_prepared_funding(
        blockchain: Arc<dyn BlockchainInterface + Send + Sync>,
        prepared: PreparedFunding,
    ) -> Result<FundingResponse, String> {
        let tx = prepared
            .txs
            .into_iter()
            .next()
            .ok_or_else(|| "No funding transaction prepared.".to_string())?;
        let mut response = FundingResponse::default();
        response.txs.push(tx.clone());
        log::info!("broadcasting funding tx {}", tx.hash().encode());
        log::debug!("funding tx hex = {}", tx_as_hexstr(&tx)?);
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

async fn resync_after_multiple_tx_failure(service: &RwLock<Service>, fund_request: &FundRequest) {
    if let Err(error) = Service::refresh_client_chain_state(service, &fund_request.client_id).await
    {
        log::warn!("failed to resync UTXO cache after partial multiple_tx funding: {error}");
    }
}

fn partial_broadcast_error(
    failed_at: u32,
    total: u32,
    broadcast: &FundingResponse,
    cause: String,
) -> String {
    let succeeded = broadcast.outpoints.len() as u32;
    let mut message =
        format!("Failed to broadcast funding transaction {failed_at} of {total}: {cause}");
    if succeeded > 0 {
        let hashes: Vec<String> = broadcast
            .outpoints
            .iter()
            .map(|outpoint| outpoint.hash.encode())
            .collect();
        message.push_str(&format!(
            ". {succeeded} transaction(s) were broadcast successfully: {}",
            hashes.join(", ")
        ));
    }
    message
}

async fn fetch_chain_state(
    blockchain: &dyn BlockchainInterface,
    address: &str,
) -> Result<(Balance, Utxo), String> {
    let balance = blockchain
        .get_balance(address)
        .await
        .map_err(|e| format!("get_balance failed: {e:?}"))?;
    let utxo = blockchain
        .get_utxo(address)
        .await
        .map_err(|e| format!("get_utxo failed: {e:?}"))?;
    Ok((balance, utxo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        test_blockchain_interface, test_config, unique_dynamic_config_path, TEST_CLIENT_ID,
    };

    #[tokio::test]
    async fn refresh_client_chain_state_restores_stale_utxo_cache() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service = RwLock::new(Service::new_for_test(&config, blockchain).await);

        {
            let mut service = service.write().await;
            let client = service
                .clients
                .iter_mut()
                .find(|client| client.client_id == TEST_CLIENT_ID)
                .expect("test client");
            client.apply_chain_state(Balance::default(), Vec::new());
        }

        Service::refresh_client_chain_state(&service, TEST_CLIENT_ID)
            .await
            .expect("refresh should succeed");

        let service = service.read().await;
        let balance = service.get_balance(TEST_CLIENT_ID).expect("test client");
        assert!(balance.confirmed > 0);
        assert!(service
            .funding_balance_error(&FundRequest {
                client_id: TEST_CLIENT_ID.to_string(),
                satoshi: 123,
                no_of_outpoints: 1,
                multiple_tx: false,
                locking_script: hex::decode(crate::test_support::LOCKING_SCRIPT_HEX).unwrap(),
            })
            .is_none());
    }

    #[test]
    fn partial_broadcast_error_lists_successful_txids() {
        use chain_gang::{messages::OutPoint, util::Hash256};

        let mut broadcast = FundingResponse::default();
        broadcast.outpoints.push(OutPoint {
            hash: Hash256::decode(
                "f67272e5c1408ecbeb8da543437c125ee1a17110317d44d13eafe31b771b795e",
            )
            .expect("valid hash"),
            index: 1,
        });

        let message = partial_broadcast_error(2, 3, &broadcast, "network error".to_string());
        assert!(message.contains("2 of 3"));
        assert!(message.contains("1 transaction(s) were broadcast successfully"));
        assert!(message.contains("f67272e5"));
    }

    #[tokio::test]
    async fn fund_with_multiple_transactions_broadcasts_each_tx_separately() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service = RwLock::new(Service::new_for_test(&config, blockchain).await);

        let fund_request = FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi: 123,
            no_of_outpoints: 2,
            multiple_tx: true,
            locking_script: hex::decode(crate::test_support::LOCKING_SCRIPT_HEX).unwrap(),
        };

        let response = Service::fund_with_multiple_transactions(&service, &fund_request)
            .await
            .expect("multiple_tx funding should succeed");
        assert_eq!(response.txs.len(), 2);
        assert_eq!(response.outpoints.len(), 2);
        assert_ne!(response.outpoints[0].hash, response.outpoints[1].hash);
    }
}
