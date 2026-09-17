use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use chain_gang::{
    interface::{Balance, BlockchainInterface, Utxo},
    messages::{OutPoint, Tx},
};
use chrono::prelude::DateTime;
use chrono::Utc;
use tokio::sync::{Mutex, RwLock};

use crate::{
    address_watcher::AddressWatcher,
    blockchain_factory::{blockchain_factory, Backend},
    broadcaster::{factory::broadcaster_factory, TxBroadcaster, MAPI_LITE},
    client::{Client, FundRequest, FundingSpendPlan},
    config::{ClientConfig, Config},
    dynamic_config::DynamicConfig,
    idempotency::{IdempotencyStore, RecordKey},
    responses::{
        BlockchainConnectionStatus, CodedError, ErrorCode, FundingResponseJson, OutpointResponse,
        StatusResponse, TxResponse,
    },
    util::tx_as_hexstr,
};

#[derive(Clone, Debug, Default)]
pub struct FundingResponse {
    pub outpoints: Vec<OutPoint>,
    pub txs: Vec<Tx>,
}

#[derive(Debug)]
pub struct MultipleTxFundError {
    pub code: ErrorCode,
    pub description: String,
    pub partial: Option<FundingResponse>,
}

impl MultipleTxFundError {
    pub fn complete(code: ErrorCode, description: impl Into<String>) -> Self {
        Self {
            code,
            description: description.into(),
            partial: None,
        }
    }

    /// Some transactions broadcast and some did not; the successful ones are
    /// carried in `partial` so the caller can return them to the client.
    pub fn partial(description: impl Into<String>, completed: FundingResponse) -> Self {
        Self {
            code: ErrorCode::PartialBroadcast,
            description: description.into(),
            partial: Some(completed),
        }
    }
}

impl FundingResponse {
    pub fn to_response(&self) -> Result<FundingResponseJson, String> {
        // The value and locking script are read from the transaction that
        // actually pays each outpoint, not echoed from the request. A client
        // signing this outpoint commits to both under BIP-143, so it needs to
        // be able to verify what was paid rather than be told what was asked
        // for.
        let mut outpoints = Vec::with_capacity(self.outpoints.len());
        for op in &self.outpoints {
            let hash = op.hash.encode();
            let tx = self
                .txs
                .iter()
                .find(|tx| tx.hash() == op.hash)
                .ok_or_else(|| format!("No transaction in response for outpoint {hash}"))?;
            let output = tx
                .outputs
                .get(op.index as usize)
                .ok_or_else(|| format!("Transaction {hash} has no output at index {}", op.index))?;
            outpoints.push(OutpointResponse {
                hash,
                index: op.index,
                satoshi: output.satoshis,
                locking_script: hex::encode(&output.lock_script.0),
            });
        }
        let mut txs = Vec::with_capacity(self.txs.len());
        for tx in &self.txs {
            txs.push(TxResponse {
                tx: tx_as_hexstr(tx)?,
            });
        }
        Ok(FundingResponseJson {
            outpoints,
            txs,
            // A freshly built response is never a replay; the flag is set on
            // the way out of the idempotency store.
            replayed: false,
        })
    }
}

#[derive(Clone)]
pub struct PreparedFunding {
    pub client_id: String,
    pub txs: Vec<Tx>,
    pub no_of_outpoints: u32,
    pub spend_plan: FundingSpendPlan,
}

/// Service data
pub struct Service {
    blockchain_status: RwLock<BlockchainConnectionStatus>,
    blockchain_update_time: RwLock<Option<SystemTime>>,
    blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
    /// Where funding transactions are sent. Wraps `blockchain_interface`
    /// unless `[mapi_lite]` is configured. See [`crate::broadcaster`].
    broadcaster: Arc<dyn TxBroadcaster>,
    clients: RwLock<HashMap<String, Arc<RwLock<Client>>>>,
    dynamic_config: Mutex<DynamicConfig>,
    admin_api_key: Option<String>,
    idempotency: Mutex<IdempotencyStore>,
    /// Set only for backends that must be told which addresses to follow.
    address_watcher: Option<Arc<dyn AddressWatcher>>,
    /// The last mapi-lite probe verdict, reused by `GET /health` until it
    /// goes stale. See [`Service::mapi_lite_health`].
    mapi_health: Mutex<Option<CachedHealth>>,
    /// How long a probe verdict is reused for. Zero without `[mapi_lite]`,
    /// where nothing is ever probed.
    mapi_health_ttl: Duration,
}

/// A mapi-lite probe verdict and the moment it was taken.
struct CachedHealth {
    taken_at: Instant,
    /// The probe's own error text, kept for the log line rather than for the
    /// `/health` body, which stays generic.
    verdict: Result<(), String>,
}

impl Service {
    fn build(
        config: &Config,
        backend: Backend,
        broadcaster: Arc<dyn TxBroadcaster>,
    ) -> Result<Service, String> {
        let Backend {
            interface: blockchain_interface,
            address_watcher,
        } = backend;
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
            broadcaster,
            clients: RwLock::new(clients),
            dynamic_config: Mutex::new(dynamic_config),
            admin_api_key: config
                .web_interface
                .admin_api_key
                .clone()
                .filter(|key| !key.is_empty()),
            idempotency: Mutex::new(IdempotencyStore::new(
                config.idempotency.ttl(),
                config.idempotency.max_entries,
            )),
            address_watcher,
            mapi_health: Mutex::new(None),
            mapi_health_ttl: config
                .mapi_lite
                .as_ref()
                .map(|mapi_lite| mapi_lite.health_timeout())
                .unwrap_or_default(),
        })
    }

    /// Claim an idempotency key for a funding request.
    ///
    /// The lock is taken and released inside this call, so it is never held
    /// across a broadcast.
    pub async fn reserve_idempotency(
        &self,
        key: RecordKey,
        fingerprint: &str,
    ) -> crate::idempotency::Reservation {
        self.idempotency.lock().await.reserve(key, fingerprint)
    }

    /// Retain a funding outcome so a retry with the same key replays it.
    pub async fn complete_idempotency(
        &self,
        key: RecordKey,
        fingerprint: &str,
        outcome: crate::idempotency::Outcome,
    ) {
        self.idempotency
            .lock()
            .await
            .complete(key, fingerprint, outcome);
    }

    /// Drop a reservation because nothing was broadcast, letting the client
    /// retry the same key.
    pub async fn release_idempotency(&self, key: &RecordKey) {
        self.idempotency.lock().await.release(key);
    }

    /// Create a new Service from the provided config
    pub async fn new(config: &Config) -> Result<Service, String> {
        let backend = blockchain_factory(config)?;

        backend.interface.status().await.map_err(|e| {
            format!("Unable to connect to blockchain, ensure that the service is running: {e}")
        })?;

        // The write path. Without [mapi_lite] it wraps the interface probed
        // just above, so only mapi-lite needs a probe of its own -- and gets
        // one, to tell the operator at startup rather than on the first /fund.
        //
        // A failed probe is reported but does not stop startup, the way a
        // refused address import does not (see
        // watch_configured_client_addresses). Refusing to start ties this
        // service's lifecycle to mapi-lite's: the container health check would
        // restart the process, the probe would fail again, and the service
        // would sit in a restart loop -- taking /status, balances and every
        // other read path, none of which need mapi-lite, down with it, and
        // unable to recover on its own. Serving degraded and saying so through
        // GET /health leaves the operator a service that heals when mapi-lite
        // comes back.
        let broadcaster = broadcaster_factory(config, Arc::clone(&backend.interface))?;
        if let Some(mapi_lite) = &config.mapi_lite {
            if let Err(e) = broadcaster.health_check().await {
                log::warn!(
                    "Unable to reach mapi-lite at {} at startup: {e}. Funding will fail until it \
                     is reachable; GET /health reports the service unhealthy meanwhile.",
                    mapi_lite.base_url()
                );
            }
        }

        let service = Self::build(config, backend, broadcaster)?;
        // Before the first balance read, so the backend already knows the
        // addresses it is about to be asked about.
        service.watch_configured_client_addresses().await;
        service.update_balances().await;
        Ok(service)
    }

    #[cfg(test)]
    pub async fn new_for_test(
        config: &Config,
        blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
    ) -> Service {
        Self::new_for_test_with_watcher(config, blockchain_interface, None).await
    }

    /// As production without `[mapi_lite]`: broadcasts go through the
    /// blockchain interface.
    #[cfg(test)]
    pub async fn new_for_test_with_watcher(
        config: &Config,
        blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
        address_watcher: Option<Arc<dyn AddressWatcher>>,
    ) -> Service {
        use crate::broadcaster::woc::WocBroadcaster;
        let broadcaster: Arc<dyn TxBroadcaster> = Arc::new(WocBroadcaster::new(
            Arc::clone(&blockchain_interface),
            &config.blockchain_interface.interface_type,
        ));
        Self::new_for_test_full(config, blockchain_interface, address_watcher, broadcaster).await
    }

    /// Reads through `blockchain_interface`, writes through `broadcaster`,
    /// as production with `[mapi_lite]` does.
    #[cfg(test)]
    pub async fn new_for_test_with_broadcaster(
        config: &Config,
        blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
        broadcaster: Arc<dyn TxBroadcaster>,
    ) -> Service {
        Self::new_for_test_full(config, blockchain_interface, None, broadcaster).await
    }

    #[cfg(test)]
    async fn new_for_test_full(
        config: &Config,
        blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
        address_watcher: Option<Arc<dyn AddressWatcher>>,
        broadcaster: Arc<dyn TxBroadcaster>,
    ) -> Service {
        blockchain_interface
            .status()
            .await
            .expect("Unable to connect to test blockchain.");

        let service = Self::build(
            config,
            Backend {
                interface: blockchain_interface,
                address_watcher,
            },
            broadcaster,
        )
        .expect("Invalid client configuration in test setup");
        service.watch_configured_client_addresses().await;
        service.update_balances().await;
        service
    }

    /// Ask the backend to watch every configured client's funding address.
    ///
    /// A failure is reported but does not stop startup: the node may refuse
    /// the import for a reason the operator already knows about, such as a
    /// descriptor wallet, and refusing to run would be worse than a warning
    /// they can act on. The warning names the consequence, because the symptom
    /// -- a zero balance from a reachable node -- does not point at its cause.
    async fn watch_configured_client_addresses(&self) {
        let Some(watcher) = self.address_watcher.as_ref() else {
            return;
        };
        for client in self.client_handles().await {
            let address = client.read().await.get_address();
            if let Err(error) = watcher.watch_address(&address).await {
                log::warn!(
                    "Could not ask the node to watch {address}: {error}. \
                     Its balance will read zero and funding will be refused until \
                     the node tracks it."
                );
            }
        }
    }

    /// As above, for one address, when a client is added at runtime.
    async fn watch_client_address(&self, client_id: &str) {
        let Some(watcher) = self.address_watcher.as_ref() else {
            return;
        };
        let Some(client) = self.client_handle(client_id).await else {
            return;
        };
        let address = client.read().await.get_address();
        if let Err(error) = watcher.watch_address(&address).await {
            log::warn!(
                "Could not ask the node to watch {address} for new client {client_id}: {error}. \
                 Its balance will read zero and funding will be refused until the node tracks it."
            );
        }
    }

    async fn client_handle(&self, client_id: &str) -> Option<Arc<RwLock<Client>>> {
        self.clients.read().await.get(client_id).cloned()
    }

    async fn client_handles(&self) -> Vec<Arc<RwLock<Client>>> {
        self.clients.read().await.values().cloned().collect()
    }

    pub async fn add_client(&self, client_config: &ClientConfig) -> Result<(), CodedError> {
        crate::secrets::warn_plaintext_client_secrets(
            &client_config.client_id,
            &client_config.wif_key,
            client_config.api_key.as_deref(),
        );
        // A bad secret reference or an unusable WIF is a problem with the
        // submitted client, not a service fault.
        let resolved = client_config
            .clone()
            .resolve_secrets()
            .map_err(|e| CodedError::new(ErrorCode::InvalidRequest, e))?;
        let new_client = Arc::new(RwLock::new(
            Client::try_new(&resolved)
                .map_err(|e| CodedError::new(ErrorCode::InvalidRequest, e))?,
        ));
        {
            let mut clients = self.clients.write().await;
            if clients.contains_key(&client_config.client_id) {
                return Err(CodedError::new(
                    ErrorCode::ClientExists,
                    format!("Client already exists: {}", client_config.client_id),
                ));
            }
            clients.insert(client_config.client_id.clone(), Arc::clone(&new_client));
        }
        // Persisting failed, so undo the in-memory insert. This one really is
        // a service fault.
        if let Err(error) = self.dynamic_config.lock().await.add(client_config) {
            self.clients.write().await.remove(&client_config.client_id);
            return Err(CodedError::internal(error));
        }
        // A client added at runtime needs watching too, or its balance reads
        // zero until the next restart.
        self.watch_client_address(&client_config.client_id).await;
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
            broadcaster: self.broadcaster.name().to_string(),
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

    /// The write path for funding transactions.
    pub fn broadcaster(&self) -> Arc<dyn TxBroadcaster> {
        Arc::clone(&self.broadcaster)
    }

    /// Whether funding transactions go through mapi-lite.
    pub fn mapi_lite_configured(&self) -> bool {
        self.broadcaster.name() == MAPI_LITE
    }

    /// Probe mapi-lite for `GET /health`, reusing a recent verdict.
    ///
    /// `None` when mapi-lite is not configured: there is then nothing to
    /// probe, and `/health` stays the pure liveness check it always was.
    ///
    /// The verdict is cached for `mapi_lite.health_timeout_seconds`, because
    /// `/health` is unauthenticated *and* exempt from the rate limiter
    /// (SR-SEC-013), so without a cache anyone who can reach the port can turn
    /// health traffic into an unbounded stream of requests to mapi-lite and
    /// starve the funding path from an endpoint that costs them nothing. One
    /// probe per TTL bounds that, and still answers the Docker health check
    /// (every 30s) with a fresh result each time.
    pub async fn mapi_lite_health(&self) -> Option<Result<(), String>> {
        if !self.mapi_lite_configured() {
            return None;
        }

        // Held across the probe on purpose: concurrent callers wait for the
        // one in flight rather than each starting their own, which is the
        // point of the cache. The probe is bounded by health_timeout.
        let mut cached = self.mapi_health.lock().await;
        if let Some(entry) = cached.as_ref() {
            if entry.taken_at.elapsed() < self.mapi_health_ttl {
                return Some(entry.verdict.clone());
            }
        }

        let verdict = self
            .broadcaster
            .health_check()
            .await
            .map_err(|e| e.to_string());
        *cached = Some(CachedHealth {
            taken_at: Instant::now(),
            verdict: verdict.clone(),
        });
        Some(verdict)
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

    #[cfg(test)]
    pub async fn set_test_chain_state(
        &self,
        client_id: &str,
        balance: Balance,
        unspent: Utxo,
    ) -> Result<(), String> {
        let client = self
            .client_handle(client_id)
            .await
            .ok_or_else(|| format!("Unknown client_id {client_id}"))?;
        client.write().await.apply_chain_state(balance, unspent);
        Ok(())
    }

    pub async fn funding_balance_error(&self, fund_request: &FundRequest) -> Option<CodedError> {
        let client = self.client_handle(&fund_request.client_id).await?;
        let guard = client.read().await;
        guard.funding_balance_error(fund_request)
    }

    /// Build and sign a funding transaction without updating the local UTXO cache.
    pub async fn prepare_funding_outpoints(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<(Arc<dyn TxBroadcaster>, PreparedFunding), String> {
        let client = service
            .client_handle(&fund_request.client_id)
            .await
            .ok_or_else(|| format!("Unknown client_id {}", fund_request.client_id))?;
        let (tx, spend_plan) = client.read().await.plan_funding_tx(fund_request)?;
        Ok((
            service.broadcaster(),
            PreparedFunding {
                client_id: fund_request.client_id.clone(),
                txs: vec![tx],
                no_of_outpoints: fund_request.no_of_outpoints,
                spend_plan,
            },
        ))
    }

    pub async fn commit_prepared_funding(
        service: &Arc<Service>,
        prepared: &PreparedFunding,
    ) -> Result<(), String> {
        let client = service
            .client_handle(&prepared.client_id)
            .await
            .ok_or_else(|| format!("Unknown client_id {}", prepared.client_id))?;
        client
            .write()
            .await
            .commit_funding_spend(prepared.spend_plan.clone());
        Ok(())
    }

    pub async fn execute_funding(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, CodedError> {
        if let Some(error) = service.funding_balance_error(fund_request).await {
            return Err(error);
        }

        let (broadcaster, prepared) = Self::prepare_funding_outpoints(service, fund_request)
            .await
            .map_err(CodedError::internal)?;
        match Self::broadcast_prepared_funding(broadcaster, &prepared).await {
            Ok(response) => {
                if let Err(description) = Self::commit_prepared_funding(service, &prepared).await {
                    log::warn!("commit_prepared_funding failed: {}", description);
                    let _ =
                        Self::refresh_client_chain_state(service, &fund_request.client_id).await;
                    return Err(CodedError::internal(description));
                }
                Ok(response)
            }
            Err(description) => {
                let _ = Self::refresh_client_chain_state(service, &fund_request.client_id).await;
                Err(description)
            }
        }
    }

    /// Fund a request with separate transactions, preparing and broadcasting one at a time.
    pub async fn fund_with_multiple_transactions(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, MultipleTxFundError> {
        if let Some(error) = service.funding_balance_error(fund_request).await {
            return Err(MultipleTxFundError::complete(error.code, error.description));
        }

        let mut combined = FundingResponse::default();
        let total = fund_request.no_of_outpoints;

        for tx_index in 0..total {
            // Each transaction pays its own script, so build the per-tx
            // request inside the loop rather than reusing one.
            let per_tx_request = FundRequest {
                client_id: fund_request.client_id.clone(),
                satoshi: fund_request.satoshi,
                no_of_outpoints: 1,
                multiple_tx: false,
                locking_scripts: vec![fund_request
                    .locking_scripts
                    .get(tx_index as usize)
                    .or_else(|| fund_request.locking_scripts.last())
                    .cloned()
                    .unwrap_or_default()],
            };
            match Self::prepare_funding_outpoints(service, &per_tx_request).await {
                Ok((broadcaster, prepared)) => {
                    match Self::broadcast_prepared_funding(broadcaster, &prepared).await {
                        Ok(partial) => {
                            if let Err(description) =
                                Self::commit_prepared_funding(service, &prepared).await
                            {
                                resync_after_multiple_tx_failure(service, fund_request).await;
                                return Err(MultipleTxFundError::complete(
                                    ErrorCode::Internal,
                                    description,
                                ));
                            }
                            combined.outpoints.extend(partial.outpoints);
                            combined.txs.extend(partial.txs);
                        }
                        Err(cause) => {
                            if tx_index == 0 {
                                resync_after_multiple_tx_failure(service, fund_request).await;
                                return Err(MultipleTxFundError::complete(
                                    cause.code,
                                    cause.description,
                                ));
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
                        return Err(MultipleTxFundError::complete(ErrorCode::Internal, cause));
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
        broadcaster: Arc<dyn TxBroadcaster>,
        prepared: &PreparedFunding,
    ) -> Result<FundingResponse, CodedError> {
        let tx = prepared
            .txs
            .first()
            .cloned()
            .ok_or_else(|| CodedError::internal("No funding transaction prepared."))?;
        let mut response = FundingResponse::default();
        response.txs.push(tx.clone());
        log::info!("broadcasting funding tx {}", tx.hash().encode());
        log::debug!(
            "funding tx hex = {}",
            tx_as_hexstr(&tx).map_err(CodedError::internal)?
        );
        // The client sees a fixed code and message whatever the upstream said;
        // the detail goes to the log, where an operator can act on it.
        broadcaster.broadcast_tx(&tx).await.map_err(|e| {
            log::warn!(
                "Failed to broadcast funding transaction via {}: {e}",
                broadcaster.name()
            );
            CodedError::new(
                ErrorCode::BroadcastFailed,
                "Failed to broadcast funding transaction.",
            )
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
    cause: impl std::fmt::Display,
) -> MultipleTxFundError {
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
        return MultipleTxFundError::partial(message, broadcast.clone());
    }
    MultipleTxFundError::complete(ErrorCode::BroadcastFailed, message)
}

async fn fetch_chain_state(
    blockchain: &dyn BlockchainInterface,
    address: &str,
) -> Result<(Balance, Utxo), String> {
    let balance = blockchain
        .get_balance(address)
        .await
        .map_err(|e| format!("get_balance failed: {e}"))?;
    let utxo = blockchain
        .get_utxo(address)
        .await
        .map_err(|e| format!("get_utxo failed: {e}"))?;
    Ok((balance, utxo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address_watcher::RecordingWatcher;
    use crate::config::ClientConfig;
    use crate::test_support::{
        test_blockchain_interface, test_config, test_config_with_keys, unique_dynamic_config_path,
        LOCKING_SCRIPT_HEX, TEST_ADDRESS, TEST_CLIENT_ID, TEST_WIF,
    };

    async fn service_with_watcher(config: &Config, watcher: Arc<RecordingWatcher>) -> Arc<Service> {
        let blockchain = test_blockchain_interface(config).await;
        Arc::new(Service::new_for_test_with_watcher(config, blockchain, Some(watcher)).await)
    }

    /// A node reports zero for an address it does not track, so the configured
    /// clients' addresses must be handed to it before the first balance read.
    #[tokio::test]
    async fn sr_bchn_008_configured_client_addresses_are_watched_at_startup() {
        let config = test_config(&unique_dynamic_config_path());
        let watcher = Arc::new(RecordingWatcher::new(false));
        let _service = service_with_watcher(&config, watcher.clone()).await;
        assert_eq!(watcher.watched(), vec![TEST_ADDRESS.to_string()]);
    }

    /// A client added through POST /client needs watching too, or its balance
    /// reads zero until the next restart.
    #[tokio::test]
    async fn sr_bchn_008_a_runtime_added_client_address_is_watched() {
        let config = test_config(&unique_dynamic_config_path());
        let watcher = Arc::new(RecordingWatcher::new(false));
        let service = service_with_watcher(&config, watcher.clone()).await;

        service
            .add_client(&ClientConfig {
                client_id: "id2".to_string(),
                wif_key: TEST_WIF.to_string(),
                api_key: None,
            })
            .await
            .expect("client should be added");

        // the startup import, then the new client's
        assert_eq!(watcher.watched().len(), 2);
        assert_eq!(watcher.watched()[1], TEST_ADDRESS);
    }

    /// An import failure is reported but must not stop the service: the node
    /// may refuse for a reason the operator already knows about.
    #[tokio::test]
    async fn sr_bchn_008_a_failed_import_does_not_prevent_startup() {
        let config = test_config(&unique_dynamic_config_path());
        let watcher = Arc::new(RecordingWatcher::new(true));
        let service = service_with_watcher(&config, watcher.clone()).await;
        assert_eq!(watcher.watched(), vec![TEST_ADDRESS.to_string()]);
        // the service is still usable
        assert!(service.is_client_id_valid(TEST_CLIENT_ID).await);
    }

    /// Backends that index the chain themselves must not be sent imports.
    #[tokio::test]
    async fn sr_bchn_007_no_watcher_means_no_import_attempts() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        // None, as woc/uaas/test supply
        let service = Service::new_for_test_with_watcher(&config, blockchain, None).await;
        assert!(service.is_client_id_valid(TEST_CLIENT_ID).await);
    }

    fn sample_fund_request(client_id: &str) -> FundRequest {
        FundRequest {
            client_id: client_id.to_string(),
            satoshi: 123,
            no_of_outpoints: 1,
            multiple_tx: false,
            locking_scripts: vec![hex::decode(LOCKING_SCRIPT_HEX).unwrap()],
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

        let error = partial_broadcast_error(2, 3, &broadcast, "network error".to_string());
        assert!(error.partial.is_some());
        assert!(error.description.contains("2 of 3"));
        assert!(error
            .description
            .contains("1 transaction(s) were broadcast successfully"));
        assert!(error.description.contains("f67272e5"));
        assert_eq!(error.partial.unwrap().outpoints.len(), 1);
    }

    #[test]
    fn partial_broadcast_error_without_successes_has_no_partial_payload() {
        let error = partial_broadcast_error(
            1,
            2,
            &FundingResponse::default(),
            "network error".to_string(),
        );
        assert!(error.partial.is_none());
        assert!(error.description.contains("network error"));
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
            locking_scripts: vec![hex::decode(LOCKING_SCRIPT_HEX).unwrap()],
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

    #[tokio::test]
    async fn concurrent_fund_requests_for_same_client_do_not_block() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service = Arc::new(Service::new_for_test(&config, blockchain).await);

        let service_a = Arc::clone(&service);
        let service_b = Arc::clone(&service);
        let request_a = sample_fund_request(TEST_CLIENT_ID);
        let mut request_b = sample_fund_request(TEST_CLIENT_ID);
        request_b.satoshi = 456;

        let (result_a, result_b) = tokio::join!(
            fund_single_transaction(&service_a, &request_a),
            fund_single_transaction(&service_b, &request_b),
        );

        result_a.expect("first same-client funding should succeed");
        result_b.expect("second same-client funding should succeed");
    }

    #[tokio::test]
    async fn sr_fund_008_partial_multiple_tx_failure_resyncs_chain_state() {
        use crate::test_support::FailingBroadcastBlockchain;

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = FailingBroadcastBlockchain::new(&config, 1).await;
        let service = Arc::new(Service::new_for_test(&config, blockchain).await);

        let fund_request = FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi: 123,
            no_of_outpoints: 2,
            multiple_tx: true,
            locking_scripts: vec![hex::decode(LOCKING_SCRIPT_HEX).unwrap()],
        };

        let error = Service::fund_with_multiple_transactions(&service, &fund_request)
            .await
            .expect_err("second broadcast should fail");
        assert!(error.partial.is_some());

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

    async fn fund_single_transaction(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, CodedError> {
        Service::refresh_client_chain_state(service, &fund_request.client_id)
            .await
            .map_err(CodedError::internal)?;
        Service::execute_funding(service, fund_request).await
    }

    /// A service reading through `blockchain` and writing through
    /// `broadcaster`, as production does with `[mapi_lite]` configured.
    async fn service_with(
        config: &Config,
        blockchain: Arc<dyn BlockchainInterface + Send + Sync>,
        broadcaster: Arc<dyn TxBroadcaster>,
    ) -> Arc<Service> {
        Arc::new(Service::new_for_test_with_broadcaster(config, blockchain, broadcaster).await)
    }

    /// With a broadcaster injected, the blockchain interface serves reads
    /// only: not one broadcast may reach it.
    #[tokio::test]
    async fn sr_bchn_009_execute_funding_uses_the_broadcaster_not_the_blockchain_interface() {
        use crate::test_support::{CountingBlockchain, CountingBroadcaster};

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = CountingBlockchain::new(&config).await;
        let broadcaster = CountingBroadcaster::new();
        let service = service_with(&config, blockchain.clone(), broadcaster.clone()).await;

        let response = fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect("funding should succeed");

        assert_eq!(response.outpoints.len(), 1);
        assert_eq!(broadcaster.broadcast_count(), 1);
        assert_eq!(blockchain.broadcast_count(), 0);
    }

    #[tokio::test]
    async fn sr_bchn_009_fund_with_multiple_transactions_uses_the_broadcaster() {
        use crate::test_support::{CountingBlockchain, CountingBroadcaster};

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = CountingBlockchain::new(&config).await;
        let broadcaster = CountingBroadcaster::new();
        let service = service_with(&config, blockchain.clone(), broadcaster.clone()).await;

        let fund_request = FundRequest {
            client_id: TEST_CLIENT_ID.to_string(),
            satoshi: 123,
            no_of_outpoints: 3,
            multiple_tx: true,
            locking_scripts: vec![hex::decode(LOCKING_SCRIPT_HEX).unwrap()],
        };
        let response = Service::fund_with_multiple_transactions(&service, &fund_request)
            .await
            .expect("multiple_tx funding should succeed");

        assert_eq!(response.txs.len(), 3);
        assert_eq!(broadcaster.broadcast_count(), 3);
        assert_eq!(blockchain.broadcast_count(), 0);
    }

    /// A broadcaster failure is reported with the same code the blockchain
    /// interface's failure always was, and leaves the UTXO cache resynced, so
    /// the client contract does not depend on which broadcaster is in use.
    #[tokio::test]
    async fn broadcaster_failure_is_broadcast_failed_and_resyncs_chain_state() {
        use crate::test_support::FailingBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service = service_with(&config, blockchain, FailingBroadcaster::new(0)).await;

        let error = fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect_err("the broadcaster fails");
        assert_eq!(error.code, ErrorCode::BroadcastFailed);

        assert!(service
            .funding_balance_error(&sample_fund_request(TEST_CLIENT_ID))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn sr_bchn_011_mapi_lite_health_is_none_without_mapi_lite() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service = Service::new_for_test(&config, blockchain).await;

        assert!(!service.mapi_lite_configured());
        assert!(service.mapi_lite_health().await.is_none());
        assert_eq!(service.get_status().await.broadcaster, "test");
    }

    #[tokio::test]
    async fn sr_bchn_011_mapi_lite_health_reports_the_probe_result() {
        use crate::test_support::StubMapiBroadcaster;

        let config = test_config(&unique_dynamic_config_path());

        let blockchain = test_blockchain_interface(&config).await;
        let healthy = service_with(&config, blockchain, StubMapiBroadcaster::new(true)).await;
        assert!(healthy.mapi_lite_configured());
        assert_eq!(healthy.mapi_lite_health().await, Some(Ok(())));
        assert_eq!(healthy.get_status().await.broadcaster, MAPI_LITE);

        let blockchain = test_blockchain_interface(&config).await;
        let unhealthy = service_with(&config, blockchain, StubMapiBroadcaster::new(false)).await;
        let probe = unhealthy.mapi_lite_health().await;
        assert!(
            matches!(probe, Some(Err(ref detail)) if detail.contains("503")),
            "{probe:?}"
        );
    }

    /// `/health` is unauthenticated and rate-limit exempt, so its probe must
    /// not reach mapi-lite once per request. Within the TTL the verdict is
    /// reused; past it a fresh probe is taken.
    #[tokio::test]
    async fn sr_bchn_011_mapi_lite_health_is_cached_for_the_health_timeout() {
        use crate::test_support::StubMapiBroadcaster;

        let mut config = test_config(&unique_dynamic_config_path());
        let mut mapi_lite = crate::config::MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        mapi_lite.health_timeout_seconds = 1;
        config.mapi_lite = Some(mapi_lite);

        let blockchain = test_blockchain_interface(&config).await;
        let broadcaster = StubMapiBroadcaster::new(true);
        let service = service_with(&config, blockchain, broadcaster.clone()).await;

        for _ in 0..5 {
            assert_eq!(service.mapi_lite_health().await, Some(Ok(())));
        }
        assert_eq!(broadcaster.probe_count(), 1, "the verdict should be reused");

        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(service.mapi_lite_health().await, Some(Ok(())));
        assert_eq!(
            broadcaster.probe_count(),
            2,
            "a stale verdict should be refreshed"
        );
    }
}
