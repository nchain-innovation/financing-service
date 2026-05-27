use std::{collections::HashMap, sync::Arc, time::SystemTime};

use chain_gang::{
    interface::{Balance, BlockchainInterface, Utxo},
    messages::{OutPoint, Tx},
};
use chrono::prelude::DateTime;
use chrono::Utc;
use tokio::sync::{Mutex, RwLock};

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
pub struct Service {
    blockchain_status: RwLock<BlockchainConnectionStatus>,
    blockchain_update_time: RwLock<Option<SystemTime>>,
    blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
    clients: RwLock<HashMap<String, Arc<RwLock<Client>>>>,
    dynamic_config: Mutex<DynamicConfig>,
    admin_api_key: Option<String>,
}

impl Service {
    fn build(
        config: &Config,
        blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
    ) -> Result<Service, String> {
        let mut clients = HashMap::new();

        if let Some(clients_config) = &config.client {
            for client_config in clients_config {
                let resolved = client_config.clone().resolve_secrets()?;
                clients.insert(
                    client_config.client_id.clone(),
                    Arc::new(RwLock::new(Client::try_new(&resolved)?)),
                );
            }
        }

        let dynamic_config = DynamicConfig::new(config);
        for client_config in &dynamic_config.contents.clients {
            crate::secrets::warn_plaintext_client_secrets(
                &client_config.client_id,
                &client_config.wif_key,
                client_config.api_key.as_deref(),
            );
            let resolved = client_config.clone().resolve_secrets()?;
            clients.insert(
                client_config.client_id.clone(),
                Arc::new(RwLock::new(Client::try_new(&resolved)?)),
            );
        }

        Ok(Service {
            blockchain_status: RwLock::new(BlockchainConnectionStatus::Unknown),
            blockchain_update_time: RwLock::new(None),
            blockchain_interface,
            clients: RwLock::new(clients),
            dynamic_config: Mutex::new(dynamic_config),
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

        let service = Self::build(config, blockchain_interface)?;
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

        let service = Self::build(config, blockchain_interface)
            .expect("Invalid client configuration in test setup");
        service.update_balances().await;
        service
    }

    async fn client_handle(&self, client_id: &str) -> Option<Arc<RwLock<Client>>> {
        self.clients.read().await.get(client_id).cloned()
    }

    async fn client_handles(&self) -> Vec<Arc<RwLock<Client>>> {
        self.clients.read().await.values().cloned().collect()
    }

    pub async fn add_client(&self, client_config: &ClientConfig) -> Result<(), String> {
        crate::secrets::warn_plaintext_client_secrets(
            &client_config.client_id,
            &client_config.wif_key,
            client_config.api_key.as_deref(),
        );
        let resolved = client_config.clone().resolve_secrets()?;
        let new_client = Arc::new(RwLock::new(Client::try_new(&resolved)?));
        {
            let mut clients = self.clients.write().await;
            if clients.contains_key(&client_config.client_id) {
                return Err(format!(
                    "Client already exists: {}",
                    client_config.client_id
                ));
            }
            clients.insert(client_config.client_id.clone(), Arc::clone(&new_client));
        }
        if let Err(error) = self.dynamic_config.lock().await.add(client_config) {
            self.clients.write().await.remove(&client_config.client_id);
            return Err(error);
        }
        Ok(())
    }

    pub async fn delete_client(&self, client_id: &str) -> Result<(), String> {
        self.dynamic_config.lock().await.remove(client_id)?;
        self.clients.write().await.remove(client_id);
        Ok(())
    }

    /// Return the Service status
    pub async fn get_status(&self) -> StatusResponse {
        let update_time = match *self.blockchain_update_time.read().await {
            Some(time) => {
                let datetime = DateTime::<Utc>::from(time);
                datetime.format("%Y-%m-%d %H:%M:%S").to_string()
            }
            None => "None".to_string(),
        };
        StatusResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            blockchain_status: *self.blockchain_status.read().await,
            blockchain_update_time: update_time,
        }
    }

    async fn get_block_headers(&self) {
        let status = match self.blockchain_interface.get_block_headers().await {
            Ok(_) => BlockchainConnectionStatus::Connected,
            Err(e) => {
                log::warn!("get_block_headers - failed {:?}", e);
                BlockchainConnectionStatus::Failed
            }
        };
        *self.blockchain_status.write().await = status;
        *self.blockchain_update_time.write().await = Some(SystemTime::now());
    }

    /// Update client balances at startup.
    pub async fn update_balances(&self) {
        let handles = self.client_handles().await;
        if handles.is_empty() {
            self.get_block_headers().await;
        } else {
            for client in handles {
                let status = match client
                    .write()
                    .await
                    .update_balance(self.blockchain_interface.as_ref())
                    .await
                {
                    Ok(_) => BlockchainConnectionStatus::Connected,
                    Err(e) => {
                        log::warn!("update_balance - failed {:?}", e);
                        BlockchainConnectionStatus::Failed
                    }
                };
                *self.blockchain_status.write().await = status;
                *self.blockchain_update_time.write().await = Some(SystemTime::now());
            }
        }
    }

    /// Refresh balances without holding client locks during blockchain I/O.
    pub async fn refresh_balances(service: &Arc<Service>) {
        let (blockchain, handles) = {
            let mut snapshot = Vec::new();
            for client in service.client_handles().await {
                snapshot.push((Arc::clone(&client), client.read().await.get_address()));
            }
            (Arc::clone(&service.blockchain_interface), snapshot)
        };

        if handles.is_empty() {
            service.get_block_headers().await;
            return;
        }

        let mut chain_updates = Vec::with_capacity(handles.len());
        for (_, address) in &handles {
            chain_updates.push(fetch_chain_state(blockchain.as_ref(), address).await);
        }

        let mut status = BlockchainConnectionStatus::Connected;
        for ((client, _), chain_state) in handles.into_iter().zip(chain_updates) {
            match chain_state {
                Ok((balance, utxo)) => client.write().await.apply_chain_state(balance, utxo),
                Err(e) => {
                    log::warn!("update_balance - failed {}", e);
                    status = BlockchainConnectionStatus::Failed;
                }
            }
        }
        *service.blockchain_status.write().await = status;
        *service.blockchain_update_time.write().await = Some(SystemTime::now());
    }

    /// Refresh one client's balance and UTXO set from the blockchain.
    pub async fn refresh_client_chain_state(
        service: &Arc<Service>,
        client_id: &str,
    ) -> Result<(), String> {
        let client = service
            .client_handle(client_id)
            .await
            .ok_or_else(|| format!("Unknown client_id {client_id}"))?;
        let address = client.read().await.get_address();
        let chain_state =
            fetch_chain_state(service.blockchain_interface.as_ref(), &address).await?;
        client
            .write()
            .await
            .apply_chain_state(chain_state.0, chain_state.1);
        *service.blockchain_status.write().await = BlockchainConnectionStatus::Connected;
        *service.blockchain_update_time.write().await = Some(SystemTime::now());
        Ok(())
    }

    pub async fn is_client_id_valid(&self, client_id: &str) -> bool {
        self.clients.read().await.contains_key(client_id)
    }

    pub async fn client_auth_required(&self, client_id: &str) -> bool {
        match self.client_handle(client_id).await {
            Some(client) => client.read().await.api_key().is_some(),
            None => false,
        }
    }

    pub async fn verify_client_api_key(&self, client_id: &str, provided: &str) -> bool {
        match self.client_handle(client_id).await {
            Some(client) => match client.read().await.api_key() {
                Some(expected) => crate::auth::constant_time_eq(provided, expected),
                None => true,
            },
            None => false,
        }
    }

    pub async fn clients_without_api_key(&self) -> Vec<String> {
        let clients = self.clients.read().await;
        let mut without = Vec::new();
        for (client_id, client) in clients.iter() {
            if client.read().await.api_key().is_none() {
                without.push(client_id.clone());
            }
        }
        without
    }

    pub async fn client_count(&self) -> usize {
        self.clients.read().await.len()
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

    pub async fn get_balance(&self, client_id: &str) -> Option<Balance> {
        Some(
            self.client_handle(client_id)
                .await?
                .read()
                .await
                .get_balance(),
        )
    }

    pub async fn get_address(&self, client_id: &str) -> Option<String> {
        Some(
            self.client_handle(client_id)
                .await?
                .read()
                .await
                .get_address(),
        )
    }

    pub async fn funding_balance_error(&self, fund_request: &FundRequest) -> Option<String> {
        let client = self.client_handle(&fund_request.client_id).await?;
        let guard = client.read().await;
        guard.funding_balance_error(fund_request)
    }

    /// Build and sign a funding transaction, updating only the target client's UTXO state.
    pub async fn prepare_funding_outpoints(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<(Arc<dyn BlockchainInterface + Send + Sync>, PreparedFunding), String> {
        let client = service
            .client_handle(&fund_request.client_id)
            .await
            .ok_or_else(|| format!("Unknown client_id {}", fund_request.client_id))?;
        let tx = client.write().await.create_funding_tx(fund_request)?;
        Ok((
            service.blockchain_interface(),
            PreparedFunding {
                txs: vec![tx],
                no_of_outpoints: fund_request.no_of_outpoints,
            },
        ))
    }

    /// Fund a request with separate transactions, preparing and broadcasting one at a time.
    pub async fn fund_with_multiple_transactions(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, String> {
        if let Some(description) = service.funding_balance_error(fund_request).await {
            return Err(description);
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
            match Self::prepare_funding_outpoints(service, &per_tx_request).await {
                Ok((blockchain, prepared)) => {
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
            }
        }

        Ok(combined)
    }

    /// Broadcast prepared funding transactions without holding client locks.
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

async fn resync_after_multiple_tx_failure(service: &Arc<Service>, fund_request: &FundRequest) {
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
    use crate::config::ClientConfig;
    use crate::test_support::{
        test_blockchain_interface, test_config, test_config_with_keys, unique_dynamic_config_path,
        LOCKING_SCRIPT_HEX, TEST_CLIENT_ID, TEST_WIF,
    };

    fn sample_fund_request(client_id: &str) -> FundRequest {
        FundRequest {
            client_id: client_id.to_string(),
            satoshi: 123,
            no_of_outpoints: 1,
            multiple_tx: false,
            locking_script: hex::decode(LOCKING_SCRIPT_HEX).unwrap(),
        }
    }

    #[tokio::test]
    async fn refresh_client_chain_state_restores_stale_balance_and_utxo_cache() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service = Arc::new(Service::new_for_test(&config, blockchain).await);

        service
            .client_handle(TEST_CLIENT_ID)
            .await
            .expect("test client")
            .write()
            .await
            .apply_chain_state(Balance::default(), Vec::new());

        Service::refresh_client_chain_state(&service, TEST_CLIENT_ID)
            .await
            .expect("refresh should succeed");

        let balance = service
            .get_balance(TEST_CLIENT_ID)
            .await
            .expect("test client");
        assert!(balance.confirmed > 0);
        assert!(service
            .funding_balance_error(&sample_fund_request(TEST_CLIENT_ID))
            .await
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
        let service = Arc::new(Service::new_for_test(&config, blockchain).await);

        let fund_request = FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi: 123,
            no_of_outpoints: 2,
            multiple_tx: true,
            locking_script: hex::decode(LOCKING_SCRIPT_HEX).unwrap(),
        };

        let response = Service::fund_with_multiple_transactions(&service, &fund_request)
            .await
            .expect("multiple_tx funding should succeed");
        assert_eq!(response.txs.len(), 2);
        assert_eq!(response.outpoints.len(), 2);
        assert_ne!(response.outpoints[0].hash, response.outpoints[1].hash);
    }

    #[tokio::test]
    async fn concurrent_fund_requests_for_different_clients_do_not_block() {
        let path = unique_dynamic_config_path();
        let config = test_config_with_keys(&path, None, None);
        let blockchain = test_blockchain_interface(&config).await;
        let service = Arc::new(Service::new_for_test(&config, blockchain).await);
        service
            .add_client(&ClientConfig {
                client_id: "client2".to_string(),
                wif_key: TEST_WIF.to_string(),
                api_key: None,
            })
            .await
            .expect("add second client");

        let service_a = Arc::clone(&service);
        let service_b = Arc::clone(&service);
        let request_a = sample_fund_request(TEST_CLIENT_ID);
        let request_b = sample_fund_request("client2");

        let (result_a, result_b) = tokio::join!(
            fund_single_transaction(&service_a, &request_a),
            fund_single_transaction(&service_b, &request_b),
        );

        result_a.expect("client1 funding should succeed");
        result_b.expect("client2 funding should succeed");
    }

    async fn fund_single_transaction(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, String> {
        Service::refresh_client_chain_state(service, &fund_request.client_id).await?;
        if let Some(description) = service.funding_balance_error(fund_request).await {
            return Err(description);
        }
        let (blockchain, prepared) =
            Service::prepare_funding_outpoints(service, fund_request).await?;
        Service::broadcast_prepared_funding(blockchain, prepared).await
    }
}
