use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
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
    broadcaster::{factory::broadcaster_factory, BroadcastError, TxBroadcaster, MAPI_LITE},
    client::{Client, FundRequest, FundingSpendPlan, InflightState},
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
    /// The last mapi-lite probe verdict, reused by `GET /ready` until it
    /// goes stale. See [`Service::mapi_lite_health`].
    mapi_health: Mutex<Option<CachedHealth>>,
    /// How long a probe verdict is reused for. Zero without `[mapi_lite]`,
    /// where nothing is ever probed.
    mapi_health_ttl: Duration,
    /// Consecutive failures reaching the blockchain interface, so a run of
    /// them reads as a run and recovery from one is announced.
    chain_health: Mutex<ChainHealth>,
    /// How old cached chain state may be before a request refreshes it.
    chain_state_max_age: Duration,
    /// One lock per client, held for the length of a chain-state refresh, so a
    /// client is never fetched twice at once -- whether the refresh was started
    /// by a request, in the background, or by the periodic sweep (SR-FUND-032).
    /// Two fetches of the same client in flight return the same answer and
    /// spend the interface's allowance twice for it.
    chain_refresh_locks: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// The rate every client currently costs its transactions at, in satoshis
    /// per kilobyte (CS-451).
    ///
    /// Held here as well as on each client so that a client added at runtime
    /// starts at the rate in force rather than the one in the config file,
    /// which a fee quote may have superseded.
    fee_satoshis_per_kb: Mutex<u64>,
    /// Whether to take the rate from mapi-lite's fee quote when there is one.
    use_mapi_fee_quote: bool,
    /// Where each client's in-flight funding state is kept across a restart
    /// (CS-465). See [`Config::inflight_state_path`].
    inflight_state_path: PathBuf,
    /// Serialises writes to that file, so two commits finishing together
    /// cannot race and leave the older snapshot on disk. Holds the ticket of
    /// the newest commit the file on disk is known to include (CS-495).
    inflight_save: Mutex<u64>,
    /// Tickets handed to commits as they ask for a save, in order. A save
    /// taken after reading this includes every commit holding a ticket up to
    /// the value read, since each commit is applied before it takes one.
    inflight_commits: std::sync::atomic::AtomicU64,
    /// How many times the file has actually been written, so a test can tell
    /// a shared save from one per commit.
    #[cfg(test)]
    inflight_writes: std::sync::atomic::AtomicU64,
}

/// Version of the in-flight state file's layout.
const INFLIGHT_FILE_VERSION: u32 = 1;

/// The on-disk form of every client's in-flight funding state (CS-465).
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct InflightFile {
    version: u32,
    #[serde(default)]
    clients: BTreeMap<String, InflightState>,
}

/// Read the in-flight state a previous run left, keyed by client id.
///
/// A missing file is normal -- first start, or nothing was ever in flight. An
/// unreadable one is logged as an error and treated as empty rather than
/// refusing to start: the service then behaves as it did before this file
/// existed, which is the degraded case CS-465 describes, and the error says
/// so. Refusing to start would take every read path down over state that only
/// matters for the next few minutes.
fn load_inflight_file(path: &Path) -> BTreeMap<String, InflightState> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => {
            log::error!(
                "cannot read in-flight funding state from {}: {e}. Starting without it, so \
                 outpoints spent shortly before this restart may be handed out again.",
                path.display()
            );
            return BTreeMap::new();
        }
    };
    match serde_json::from_str::<InflightFile>(&text) {
        Ok(file) if file.version == INFLIGHT_FILE_VERSION => file.clients,
        Ok(file) => {
            log::error!(
                "in-flight funding state in {} is version {}, expected {}. Starting without it.",
                path.display(),
                file.version,
                INFLIGHT_FILE_VERSION
            );
            BTreeMap::new()
        }
        Err(e) => {
            log::error!(
                "in-flight funding state in {} is not readable: {e}. Starting without it, so \
                 outpoints spent shortly before this restart may be handed out again.",
                path.display()
            );
            BTreeMap::new()
        }
    }
}

/// Write `file` to `path` so that a reader never sees half of it: into a
/// sibling first, flushed, then renamed over the original.
///
/// Compact rather than pretty-printed: it is written on the request path, and
/// pretty-printing roughly doubles what has to be serialised and flushed
/// (CS-495). `jq . <file>` reads it just as well.
fn write_inflight_file(path: &Path, file: &InflightFile) -> std::io::Result<()> {
    use std::io::Write;
    let text = serde_json::to_string(file).map_err(std::io::Error::other)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    {
        let mut out = std::fs::File::create(&temporary)?;
        out.write_all(text.as_bytes())?;
        out.sync_all()?;
    }
    std::fs::rename(&temporary, path)
}

/// A run of failures talking to the blockchain interface.
///
/// The bug report's complaint was not only that refreshes failed, but that
/// nothing said when they stopped failing: the warnings were "the last log
/// lines", leaving no way to tell a service that had recovered from one still
/// in trouble. Counting the run gives each warning the length of it, and lets
/// recovery be announced once.
#[derive(Default)]
struct ChainHealth {
    consecutive_failures: u64,
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
        let inflight_state_path = config.inflight_state_path();
        let mut inflight = load_inflight_file(&inflight_state_path);
        let mut restore = |client_id: &str, client: &mut Client| {
            if let Some(state) = inflight.remove(client_id) {
                log::info!(
                    "restoring {} reserved outpoint(s) and {} pending change output(s) for \
                     {client_id} from before the restart",
                    state.reserved.len(),
                    state.pending_change.len()
                );
                client.restore_inflight_state(state);
            }
        };

        if let Some(clients_config) = &config.client {
            for client_config in clients_config {
                let resolved = client_config.clone().resolve_secrets()?;
                let mut client = Client::try_new(&resolved)?;
                client.set_fee_satoshis_per_kb(config.fees.satoshis_per_kb);
                restore(&client_config.client_id, &mut client);
                clients.insert(
                    client_config.client_id.clone(),
                    Arc::new(RwLock::new(client)),
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
            let mut client = Client::try_new(&resolved)?;
            client.set_fee_satoshis_per_kb(config.fees.satoshis_per_kb);
            restore(&client_config.client_id, &mut client);
            clients.insert(
                client_config.client_id.clone(),
                Arc::new(RwLock::new(client)),
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
            chain_health: Mutex::new(ChainHealth::default()),
            chain_state_max_age: config.service.chain_state_max_age(),
            chain_refresh_locks: std::sync::Mutex::new(HashMap::new()),
            fee_satoshis_per_kb: Mutex::new(config.fees.satoshis_per_kb),
            use_mapi_fee_quote: config.fees.use_mapi_fee_quote,
            inflight_state_path,
            inflight_save: Mutex::new(0),
            inflight_commits: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            inflight_writes: std::sync::atomic::AtomicU64::new(0),
            mapi_health_ttl: config
                .mapi_lite
                .as_ref()
                .map(|mapi_lite| mapi_lite.health_timeout())
                .unwrap_or_default(),
        })
    }

    /// Note that the blockchain interface answered.
    ///
    /// Announces recovery once, naming how long the run of failures was. The
    /// bug report asked for exactly this: without it, warnings simply stop and
    /// an operator cannot tell a recovered service from a stuck one.
    /// Returns the length of the run it recovered from, so the transition is
    /// testable without reading the log it also writes.
    async fn record_chain_success(&self) -> Option<u64> {
        let mut health = self.chain_health.lock().await;
        if health.consecutive_failures == 0 {
            return None;
        }
        let recovered_from = health.consecutive_failures;
        health.consecutive_failures = 0;
        log::info!("blockchain interface recovered after {recovered_from} consecutive failure(s)");
        Some(recovered_from)
    }

    /// Length of the current run of failures. Zero when the last attempt
    /// worked.
    #[cfg(test)]
    async fn consecutive_chain_failures(&self) -> u64 {
        self.chain_health.lock().await.consecutive_failures
    }

    /// Note that the blockchain interface did not answer, and say so with the
    /// length of the run so far.
    async fn record_chain_failure(&self, error: &str) {
        let mut health = self.chain_health.lock().await;
        health.consecutive_failures += 1;
        log::warn!(
            "blockchain interface failure #{}: {error}",
            health.consecutive_failures
        );
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
        // GET /ready leaves the operator a service that heals when mapi-lite
        // comes back.
        let broadcaster = broadcaster_factory(config, Arc::clone(&backend.interface))?;
        if let Some(mapi_lite) = &config.mapi_lite {
            if let Err(e) = broadcaster.health_check().await {
                log::warn!(
                    "Unable to reach mapi-lite at {} at startup: {e}. Funding will fail until it \
                     is reachable; GET /ready reports the service unready meanwhile.",
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

    /// The lock that serialises chain-state refreshes of `client_id`.
    ///
    /// A std mutex guards the map, never held across an await; the lock it
    /// hands out is tokio's, held across the fetch.
    fn chain_refresh_lock(&self, client_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .chain_refresh_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(locks.entry(client_id.to_string()).or_default())
    }

    /// Write every client's in-flight funding state to disk (CS-465), or wait
    /// for a write that already includes the caller's commit (CS-495).
    ///
    /// Called after each commit, so what is on disk is never behind what has
    /// been broadcast by more than the moment between the two. A failure is
    /// logged, not returned: the transaction is already on the network, and
    /// the caller has nothing it could do with the error.
    ///
    /// Commits share writes. Each takes a ticket, then queues for the save
    /// lock; whoever holds it snapshots every client and writes, and that
    /// snapshot includes every commit ticketed before it was taken. A commit
    /// that reaches the lock to find its ticket already written returns at
    /// once. So the file is still written before any caller is answered, but
    /// a burst of commits costs one write between them, not one each -- each
    /// write serialises and flushes the whole state, and doing that per commit
    /// is what capped a client at the rate one write takes.
    ///
    /// The snapshot is taken under the save lock, not before it. Two commits
    /// finishing together would otherwise each snapshot, then write in either
    /// order -- and whichever snapshot is older could land last.
    pub async fn save_inflight_state(&self) {
        use std::sync::atomic::Ordering;

        // Taken after the caller applied its commit, so any snapshot that
        // reads the counter first and the clients second includes it.
        let ticket = self.inflight_commits.fetch_add(1, Ordering::SeqCst) + 1;
        let mut written_through = self.inflight_save.lock().await;
        if *written_through >= ticket {
            return;
        }
        let covers = self.inflight_commits.load(Ordering::SeqCst);

        let handles: Vec<(String, Arc<RwLock<Client>>)> = self
            .clients
            .read()
            .await
            .iter()
            .map(|(client_id, client)| (client_id.clone(), Arc::clone(client)))
            .collect();

        let mut clients = BTreeMap::new();
        for (client_id, client) in handles {
            let state = client.read().await.inflight_state();
            if !state.is_empty() {
                clients.insert(client_id, state);
            }
        }
        let file = InflightFile {
            version: INFLIGHT_FILE_VERSION,
            clients,
        };

        // Off the async worker: the flush can take milliseconds, and the
        // worker has other requests to serve meanwhile.
        let path = self.inflight_state_path.clone();
        let written = tokio::task::spawn_blocking(move || write_inflight_file(&path, &file))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e)));
        #[cfg(test)]
        self.inflight_writes.fetch_add(1, Ordering::SeqCst);
        match written {
            Ok(()) => *written_through = covers,
            // Not marked written, so the next commit tries again rather than
            // being told a failed write covered it.
            Err(e) => log::error!(
                "cannot save in-flight funding state to {}: {e}. A restart before the chain \
                 catches up could hand the same outpoints out again.",
                self.inflight_state_path.display()
            ),
        }
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
        let mut client = Client::try_new(&resolved)
            .map_err(|e| CodedError::new(ErrorCode::InvalidRequest, e))?;
        // The rate in force, which a fee quote may have moved on from what the
        // config file says, so a client added now costs its transactions the
        // same as one that has been here since startup.
        client.set_fee_satoshis_per_kb(*self.fee_satoshis_per_kb.lock().await);
        let new_client = Arc::new(RwLock::new(client));
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
                    Ok(_) => {
                        let _ = self.record_chain_success().await;
                        BlockchainConnectionStatus::Connected
                    }
                    Err(e) => {
                        self.record_chain_failure(&format!("update_balance: {e:?}"))
                            .await;
                        BlockchainConnectionStatus::Failed
                    }
                };
                *self.blockchain_status.write().await = status;
                *self.blockchain_update_time.write().await = Some(SystemTime::now());
            }
        }
    }

    /// Refresh balances without holding client locks during blockchain I/O.
    /// Take the fee rate from mapi-lite's quote, if it offers one, and apply
    /// it to every client (CS-451).
    ///
    /// Called on the same sweep that refreshes balances rather than on the
    /// funding path, so that costing a transaction never waits on a request to
    /// mapi-lite -- CS-431 had just finished taking such calls off that path.
    /// The cost is that the rate can be up to one sweep out of date, which for
    /// a fee quote is a good trade.
    ///
    /// Any failure leaves the current rate alone, so a mapi-lite that is down
    /// stops the rate changing rather than stopping funding.
    pub async fn refresh_fee_rate(&self) {
        if !self.use_mapi_fee_quote {
            return;
        }
        let Some(rate) = self.broadcaster.fee_satoshis_per_kb().await else {
            return;
        };
        if rate == 0 {
            return;
        }
        let changed = {
            let mut current = self.fee_satoshis_per_kb.lock().await;
            let changed = *current != rate;
            *current = rate;
            changed
        };
        if changed {
            log::info!("fee rate now {rate} sat/KB, from the mapi-lite fee quote");
        }
        for client in self.client_handles().await {
            client.write().await.set_fee_satoshis_per_kb(rate);
        }
    }

    /// The rate transactions are currently costed at, in satoshis per KB.
    #[cfg(test)]
    pub async fn fee_satoshis_per_kb(&self) -> u64 {
        *self.fee_satoshis_per_kb.lock().await
    }

    pub async fn refresh_balances(service: &Arc<Service>) {
        service.refresh_fee_rate().await;
        let (blockchain, handles) = {
            let clients: Vec<(String, Arc<RwLock<Client>>)> = service
                .clients
                .read()
                .await
                .iter()
                .map(|(id, client)| (id.clone(), Arc::clone(client)))
                .collect();
            let mut snapshot = Vec::with_capacity(clients.len());
            for (client_id, client) in clients {
                let address = client.read().await.get_address();
                snapshot.push((client_id, client, address));
            }
            (Arc::clone(&service.blockchain_interface), snapshot)
        };

        if handles.is_empty() {
            service.get_block_headers().await;
            return;
        }

        // Skip whatever a request refreshed inside the window. At the default
        // -- the window equal to this period -- a client touched by traffic
        // since the last tick is already current, so the sweep pays only for
        // the quiet ones.
        //
        // Skip, too, a client whose refresh is already running -- a request's,
        // or one started in the background (SR-FUND-032). It will bring that
        // client up to date, and fetching alongside it would spend the
        // allowance twice for one answer. Each client is applied as soon as it
        // is fetched and its lock released, so a request waiting on one client
        // does not wait for the whole sweep.
        let max_age = service.chain_state_max_age;
        let mut status = BlockchainConnectionStatus::Connected;
        for (client_id, client, address) in handles {
            if client.read().await.chain_state_is_fresh(max_age) {
                continue;
            }
            let Ok(_refreshing) = service.chain_refresh_lock(&client_id).try_lock_owned() else {
                continue;
            };
            match fetch_chain_state(blockchain.as_ref(), &address).await {
                Ok(utxo) => client.write().await.apply_chain_state(utxo),
                Err(e) => {
                    service.record_chain_failure(&e).await;
                    status = BlockchainConnectionStatus::Failed;
                }
            }
        }
        if matches!(status, BlockchainConnectionStatus::Connected) {
            let _ = service.record_chain_success().await;
        }
        *service.blockchain_status.write().await = status;
        *service.blockchain_update_time.write().await = Some(SystemTime::now());
    }

    /// Refresh one client's chain state unless the cache is still fresh.
    ///
    /// The refresh before building a funding transaction exists so the service
    /// does not select an input something else has already spent. Fetching it
    /// again when the periodic refresh took it moments ago buys nothing
    /// against that, and it is the difference between one request per client
    /// per period and one per request: on a busy service the second is what
    /// runs into the rate limit and makes funding calls queue behind it.
    ///
    /// The window is `service.chain_state_max_age_seconds`, defaulting to
    /// `utxo_refresh_period`. Wider is cheaper and staler, and the trade is
    /// the operator's -- see the note on the config field.
    ///
    /// A failed broadcast marks the state stale (see
    /// [`Service::invalidate_chain_state`]), so a conflict caused by building
    /// on a stale view costs one attempt and not more.
    pub async fn refresh_client_chain_state_if_stale(
        service: &Arc<Service>,
        client_id: &str,
    ) -> Result<(), String> {
        let client = service
            .client_handle(client_id)
            .await
            .ok_or_else(|| format!("Unknown client_id {client_id}"))?;
        if client
            .read()
            .await
            .chain_state_is_fresh(service.chain_state_max_age)
        {
            return Ok(());
        }
        let lock = service.chain_refresh_lock(client_id);
        let _refreshing = lock.lock().await;
        // Checked again under the lock. Every request that found the state
        // stale queues here; the first through refreshes it, and the rest find
        // it fresh and reuse that answer instead of each fetching the same one
        // behind the rate limit (SR-FUND-032). A zero window still refreshes
        // for every request, one after another.
        if client
            .read()
            .await
            .chain_state_is_fresh(service.chain_state_max_age)
        {
            return Ok(());
        }
        Self::fetch_and_apply_chain_state(service, client_id).await
    }

    /// Make sure a funding request has chain state it may build on.
    ///
    /// With `[mapi_lite]` configured, a request whose cache is usable but
    /// older than the window does not wait for the refresh: it is funded from
    /// the cached state, and the refresh runs behind it (SR-FUND-031). The
    /// broadcast goes to mapi-lite, so the request has no other reason to touch
    /// the blockchain interface -- and on WhatsOnChain that interface is paced
    /// at a few requests a second, which is what made funding calls queue.
    ///
    /// It still waits when the cache is not usable: before the client's state
    /// has ever been loaded, and after a refused or uncertain broadcast has
    /// marked it untrustworthy. Building on that view is the risk the refresh
    /// exists to prevent.
    ///
    /// Without `[mapi_lite]` nothing changes. The broadcast itself goes through
    /// the paced interface (SR-BCHN-013), so the request waits on it either
    /// way, and a fresh view is worth the same wait.
    pub async fn prepare_chain_state_for_funding(
        service: &Arc<Service>,
        client_id: &str,
    ) -> Result<(), String> {
        if service.mapi_lite_configured() {
            let client = service
                .client_handle(client_id)
                .await
                .ok_or_else(|| format!("Unknown client_id {client_id}"))?;
            let (usable, fresh) = {
                let client = client.read().await;
                (
                    client.chain_state_usable(),
                    client.chain_state_is_fresh(service.chain_state_max_age),
                )
            };
            if usable {
                if !fresh {
                    Self::refresh_chain_state_in_background(service, client_id);
                }
                return Ok(());
            }
        }
        Self::refresh_client_chain_state_if_stale(service, client_id).await
    }

    /// Start a refresh of `client_id` that nobody waits for, unless one is
    /// already running.
    ///
    /// The lock is taken before the task is spawned, so a burst of requests
    /// that all find the state stale starts one refresh, not one each. A
    /// refresh already in flight -- another request's, an earlier background
    /// one, or the periodic sweep's -- will bring the cache up to date, so this
    /// has nothing to add and does nothing.
    fn refresh_chain_state_in_background(service: &Arc<Service>, client_id: &str) {
        let Ok(refreshing) = service.chain_refresh_lock(client_id).try_lock_owned() else {
            return;
        };
        let service = Arc::clone(service);
        let client_id = client_id.to_string();
        tokio::spawn(async move {
            let _refreshing = refreshing;
            // A refresh that finished between the caller's check and taking
            // the lock has already done this one's work.
            let fresh = match service.client_handle(&client_id).await {
                Some(client) => client
                    .read()
                    .await
                    .chain_state_is_fresh(service.chain_state_max_age),
                None => return,
            };
            if fresh {
                return;
            }
            if let Err(e) = Self::fetch_and_apply_chain_state(&service, &client_id).await {
                // Already recorded as a chain failure. The request this was
                // started for has been funded from the cache, so there is
                // nobody to return it to.
                log::warn!("background refresh_client_chain_state failed: {e}");
            }
        });
    }

    /// Mark a client's cached chain state stale, so the next request refreshes
    /// it.
    ///
    /// Cheaper than refreshing here: the caller is already being told its
    /// funding failed and cannot use a fresh answer, and if no further request
    /// arrives the fetch is never made at all.
    async fn invalidate_chain_state(service: &Arc<Service>, client_id: &str) {
        if let Some(client) = service.client_handle(client_id).await {
            client.write().await.invalidate_chain_state();
        }
    }

    /// Refresh one client's balance and UTXO set from the blockchain.
    ///
    /// Waits for any refresh of the same client already in flight, then
    /// fetches regardless: callers of this one want a new answer, not a recent
    /// one.
    pub async fn refresh_client_chain_state(
        service: &Arc<Service>,
        client_id: &str,
    ) -> Result<(), String> {
        let lock = service.chain_refresh_lock(client_id);
        let _refreshing = lock.lock().await;
        Self::fetch_and_apply_chain_state(service, client_id).await
    }

    /// The refresh itself. The caller holds the client's refresh lock.
    async fn fetch_and_apply_chain_state(
        service: &Arc<Service>,
        client_id: &str,
    ) -> Result<(), String> {
        let client = service
            .client_handle(client_id)
            .await
            .ok_or_else(|| format!("Unknown client_id {client_id}"))?;
        let address = client.read().await.get_address();
        let chain_state =
            match fetch_chain_state(service.blockchain_interface.as_ref(), &address).await {
                Ok(state) => {
                    service.record_chain_success().await;
                    state
                }
                Err(e) => {
                    service.record_chain_failure(&e).await;
                    return Err(e);
                }
            };
        client.write().await.apply_chain_state(chain_state);
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

    /// Probe mapi-lite for `GET /ready`, reusing a recent verdict.
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

    /// Balance alone. `GET /balance` reports the fundable maximum beside it
    /// and uses [`Service::get_balance_and_max_fundable`]; this stays for the
    /// tests that only care about the balance.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn get_balance(&self, client_id: &str) -> Option<Balance> {
        Some(
            self.client_handle(client_id)
                .await?
                .read()
                .await
                .get_balance(),
        )
    }

    /// Balance together with the most a single `POST /fund` could ask for.
    ///
    /// Read under one lock so the two cannot disagree: a refresh between two
    /// reads would let the endpoint report a maximum that does not belong to
    /// the balance beside it.
    pub async fn get_balance_and_max_fundable(&self, client_id: &str) -> Option<(Balance, i64)> {
        let client = self.client_handle(client_id).await?;
        let client = client.read().await;
        Some((client.get_balance(), client.max_fundable_p2pkh()))
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
    pub async fn set_test_chain_state(&self, client_id: &str, unspent: Utxo) -> Result<(), String> {
        let client = self
            .client_handle(client_id)
            .await
            .ok_or_else(|| format!("Unknown client_id {client_id}"))?;
        client.write().await.apply_chain_state(unspent);
        Ok(())
    }

    pub async fn funding_balance_error(&self, fund_request: &FundRequest) -> Option<CodedError> {
        let client = self.client_handle(&fund_request.client_id).await?;
        let guard = client.read().await;
        guard.funding_balance_error(fund_request)
    }

    /// Build and sign a funding transaction, and claim its inputs so that no
    /// concurrent request for the same client can spend them (CS-473).
    ///
    /// The balance check, the plan and the claim happen under one write lock,
    /// held only while they run -- selection, building and signing, no network
    /// -- and released before the broadcast. Requests for one client therefore
    /// still overlap on the part that takes time, but no two of them can pick
    /// the same input: planning used to take a shared lock, so every request
    /// in flight planned against the same cache and chose the same smallest
    /// suitable UTXO.
    ///
    /// The check is made inside the lock rather than before it, so a request
    /// cannot pass it, lose its UTXO to a concurrent claim, and then fail to
    /// plan. It gets the coded error the check gives -- the wallet's funds are
    /// all in flight -- rather than an internal one.
    pub async fn prepare_funding_outpoints(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<(Arc<dyn TxBroadcaster>, PreparedFunding), CodedError> {
        let client = service
            .client_handle(&fund_request.client_id)
            .await
            .ok_or_else(|| {
                CodedError::internal(format!("Unknown client_id {}", fund_request.client_id))
            })?;
        let (tx, spend_plan) = {
            let mut client = client.write().await;
            if let Some(error) = client.funding_balance_error(fund_request) {
                return Err(error);
            }
            let (tx, plan) = client
                .plan_funding_tx(fund_request)
                .map_err(CodedError::internal)?;
            client.claim_inputs(&plan);
            (tx, plan)
        };
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
            .commit_funding_spend(prepared.spend_plan.clone())?;
        service.save_inflight_state().await;
        Ok(())
    }

    pub async fn execute_funding(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, CodedError> {
        let (broadcaster, prepared) =
            Self::prepare_funding_outpoints(service, fund_request).await?;
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
            Err(error) => {
                if error.code == ErrorCode::BroadcastOutcomeUnknown {
                    Self::reserve_uncertain_funding(service, &prepared).await;
                } else {
                    Self::release_prepared_funding(service, &prepared).await;
                }
                // Marked stale rather than refetched. The cache is already
                // right -- nothing was spent on a refusal, and an uncertain
                // outcome has just reserved its inputs -- so a request here
                // would buy nothing for this caller. What it would buy is a
                // second opinion for the *next* caller, if the refusal was a
                // conflict, and marking the state stale gets that without
                // paying for it when no next caller arrives.
                Self::invalidate_chain_state(service, &fund_request.client_id).await;
                Err(error)
            }
        }
    }

    /// Treat a prepared funding's inputs as spent although the broadcast
    /// outcome is unknown.
    ///
    /// Called instead of [`Self::commit_prepared_funding`] when the
    /// broadcaster could not say what became of the transaction. Failure to
    /// apply it is logged rather than returned: the caller is already being
    /// told its funding did not complete, and there is nothing it could do
    /// with a second error.
    /// Give back the inputs a prepared funding claimed, after a broadcast the
    /// upstream definitely did not take (CS-473). Nothing was spent, so they
    /// return to the cache at once rather than sitting reserved.
    async fn release_prepared_funding(service: &Arc<Service>, prepared: &PreparedFunding) {
        if let Some(client) = service.client_handle(&prepared.client_id).await {
            client.write().await.release_claim(&prepared.spend_plan);
        }
    }

    async fn reserve_uncertain_funding(service: &Arc<Service>, prepared: &PreparedFunding) {
        match service.client_handle(&prepared.client_id).await {
            Some(client) => {
                client
                    .write()
                    .await
                    .commit_uncertain_funding_spend(prepared.spend_plan.clone());
                service.save_inflight_state().await;
            }
            None => log::warn!(
                "cannot reserve the inputs of an uncertain funding transaction: unknown client_id \
                 {}",
                prepared.client_id
            ),
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
                            if cause.code == ErrorCode::BroadcastOutcomeUnknown {
                                Self::reserve_uncertain_funding(service, &prepared).await;
                            } else {
                                Self::release_prepared_funding(service, &prepared).await;
                            }
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
                        return Err(MultipleTxFundError::complete(cause.code, cause.description));
                    }
                    resync_after_multiple_tx_failure(service, fund_request).await;
                    return Err(partial_broadcast_error(
                        tx_index + 1,
                        total,
                        &combined,
                        cause.description,
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
        // One line before and one after every broadcast, each naming the
        // transaction, so a txid found anywhere -- a caller's report, a block
        // explorer -- leads to what happened to it (CS-482). `funding tx
        // <txid>` is common to every line about a transaction, so one grep
        // finds them all. The attempt line stays even though an outcome line
        // follows: for an outcome that is unknown it is the only record that
        // the transaction left.
        let txid = tx.hash().encode();
        log::info!(
            "broadcasting funding tx {txid} (client_id={}, outpoints={}, broadcaster={})",
            prepared.client_id,
            prepared.no_of_outpoints,
            broadcaster.name()
        );
        log::debug!(
            "funding tx {txid} hex={}",
            tx_as_hexstr(&tx).map_err(CodedError::internal)?
        );
        // The client sees a fixed message whatever the upstream said; the
        // detail, including the upstream's own reason and its view of whether
        // a resubmission could work, goes to the log through `BroadcastError`'s
        // Display.
        if let Err(e) = broadcaster.broadcast_tx(&tx).await {
            let error = coded_broadcast_error(&e);
            log::warn!(
                "broadcast failed: funding tx {txid} (class={}, broadcaster={}): {e}",
                broadcast_failure_class(error.code),
                broadcaster.name()
            );
            return Err(error);
        }
        log::info!(
            "broadcast accepted: funding tx {txid} (broadcaster={})",
            broadcaster.name()
        );
        let hash = tx.hash();
        // The funded outputs are the last `no_of_outpoints` of the
        // transaction. They used to be assumed to start at index 1, because a
        // change output always sat at index 0; since CS-452 a transaction
        // whose change would have been dust has no change output, and they
        // start at 0 instead. Derived rather than assumed, so it stays right
        // either way.
        let first = tx.outputs.len() as u32 - prepared.no_of_outpoints;
        response.outpoints = (first..first + prepared.no_of_outpoints)
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

/// The client's unspent set, which the balance is then derived from.
///
/// One request, not two. A separate balance query can disagree with the
/// unspent set -- WhatsOnChain's is deprecated and under-reports unconfirmed
/// outputs -- and the unspent set is the one that decides what can be funded,
/// so it is the one that is asked for. Halving the requests per refresh also
/// halves what the rate limit has to cover.
async fn fetch_chain_state(
    blockchain: &dyn BlockchainInterface,
    address: &str,
) -> Result<Utxo, String> {
    blockchain
        .get_utxo(address)
        .await
        .map_err(|e| format!("get_utxo failed: {e}"))
}

/// The answer a caller gets for a broadcast that did not succeed.
///
/// The code carries the distinctions the caller's next move depends on, and
/// the three are genuinely different moves: a broadcast that failed spent
/// nothing and may work on retry; one that was refused outright spent nothing
/// and will never work on retry; one whose outcome is unknown may have spent
/// everything.
fn coded_broadcast_error(error: &BroadcastError) -> CodedError {
    match error {
        BroadcastError::Indeterminate(_) => CodedError::new(
            ErrorCode::BroadcastOutcomeUnknown,
            "The funding transaction was sent and its outcome is unknown; it may be on the \
             network. Do not retry with a new idempotency_key, which would risk funding twice.",
        ),
        // The upstream looked at this transaction and will not take it however
        // many times it is offered.
        BroadcastError::Rejected {
            retryable: false, ..
        } => CodedError::new(
            ErrorCode::BroadcastRejected,
            "The upstream refused the funding transaction and will not accept it on retry. \
             See the service log for its reason.",
        ),
        // Unreachable, or refused in a way the upstream itself called worth
        // retrying. Either way nothing was spent.
        _ => CodedError::new(
            ErrorCode::BroadcastFailed,
            "Failed to broadcast funding transaction.",
        ),
    }
}

/// The class a failed broadcast is logged with (CS-482).
///
/// Read off the code the caller is answered with rather than off the
/// upstream's error, so the log and the response cannot disagree: a refusal
/// the upstream called retryable is answered `broadcast_failed`, and is logged
/// as failed, not rejected.
fn broadcast_failure_class(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::BroadcastRejected => "rejected",
        ErrorCode::BroadcastOutcomeUnknown => "indeterminate",
        _ => "failed",
    }
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
            .apply_chain_state(Vec::new());

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

    /// Stands in for the `POST /fund` handler, which refreshes chain state
    /// only when the cache has gone stale before it builds anything.
    async fn fund_single_transaction(
        service: &Arc<Service>,
        fund_request: &FundRequest,
    ) -> Result<FundingResponse, CodedError> {
        // The fund handler's path, so tests exercise what a request does.
        Service::prepare_chain_state_for_funding(service, &fund_request.client_id)
            .await
            .map_err(CodedError::internal)?;
        Service::execute_funding(service, fund_request).await
    }

    /// CS-418 asked for a line saying the interface came back. The reporter's
    /// complaint was that warnings were "the last log lines", leaving no way
    /// to tell a recovered service from a stuck one -- so recovery is
    /// announced once, with the length of the run, and only when there was
    /// something to recover from.
    #[tokio::test]
    async fn cs_418_recovery_is_announced_once_after_a_run_of_failures() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service = Arc::new(Service::new_for_test(&config, blockchain).await);

        // a healthy service announces nothing
        assert_eq!(service.consecutive_chain_failures().await, 0);
        assert_eq!(service.record_chain_success().await, None);

        for _ in 0..3 {
            service.record_chain_failure("429 Too Many Requests").await;
        }
        assert_eq!(service.consecutive_chain_failures().await, 3);

        // recovery names the run, once
        assert_eq!(service.record_chain_success().await, Some(3));
        assert_eq!(service.consecutive_chain_failures().await, 0);
        assert_eq!(
            service.record_chain_success().await,
            None,
            "a second success must not announce recovery again"
        );
    }

    // ---- CS-431: fewer refreshes, not just slower ones ----

    use crate::test_support::CountingBlockchain;

    /// A service whose cached chain state stays fresh for `seconds`, reading
    /// through a counter so refreshes can be counted at the far end.
    async fn service_with_freshness(seconds: u64) -> (Arc<Service>, Arc<CountingBlockchain>) {
        let mut config = test_config(&unique_dynamic_config_path());
        config.service.utxo_refresh_period = seconds;
        let blockchain = CountingBlockchain::new(&config).await;
        let service = Arc::new(Service::new_for_test(&config, blockchain.clone()).await);
        (service, blockchain)
    }

    /// The saving the ticket is about. Repeated requests inside the window
    /// reuse what the last refresh fetched instead of each fetching the same
    /// answer, which is what made a burst queue behind the rate limiter.
    #[tokio::test]
    async fn cs_431_requests_inside_the_window_reuse_the_cached_state() {
        let (service, blockchain) = service_with_freshness(60).await;
        Service::refresh_client_chain_state(&service, TEST_CLIENT_ID)
            .await
            .expect("first refresh");
        let after_first = blockchain.utxo_read_count();

        for _ in 0..5 {
            Service::refresh_client_chain_state_if_stale(&service, TEST_CLIENT_ID)
                .await
                .expect("cached");
        }

        assert_eq!(
            blockchain.utxo_read_count(),
            after_first,
            "five requests inside the window should not have fetched again"
        );
    }

    /// The window is a window, not a switch: once it passes, the next request
    /// fetches. Zero means every request refreshes, which is what the service
    /// did before this change and what an operator gets by setting it.
    #[tokio::test]
    async fn cs_431_a_zero_window_refreshes_on_every_request() {
        let (service, blockchain) = service_with_freshness(0).await;
        let before = blockchain.utxo_read_count();
        for _ in 0..3 {
            Service::refresh_client_chain_state_if_stale(&service, TEST_CLIENT_ID)
                .await
                .expect("refresh");
        }
        assert_eq!(
            blockchain.utxo_read_count(),
            before + 3,
            "a zero window must not skip anything"
        );
    }

    /// The mitigation for a wide window. Building on a stale view risks
    /// selecting an input something else has spent, and the upstream refusing
    /// the result is the evidence. That refusal marks the cache stale, so the
    /// next attempt refreshes rather than repeating the mistake -- and it
    /// costs nothing when no next attempt comes.
    #[tokio::test]
    async fn cs_431_a_refused_broadcast_makes_the_next_request_refresh() {
        use crate::test_support::RejectingBroadcaster;

        let mut config = test_config(&unique_dynamic_config_path());
        config.service.utxo_refresh_period = 3600;
        let blockchain = CountingBlockchain::new(&config).await;
        let service = service_with(
            &config,
            blockchain.clone(),
            RejectingBroadcaster::new(false),
        )
        .await;

        Service::refresh_client_chain_state(&service, TEST_CLIENT_ID)
            .await
            .expect("first refresh");
        let before = blockchain.utxo_read_count();

        // inside a one-hour window, so nothing would refresh on age alone
        Service::refresh_client_chain_state_if_stale(&service, TEST_CLIENT_ID)
            .await
            .expect("cached");
        assert_eq!(blockchain.utxo_read_count(), before, "still fresh");

        let error = fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect_err("the upstream refuses it");
        assert_eq!(error.code, ErrorCode::BroadcastRejected);

        Service::refresh_client_chain_state_if_stale(&service, TEST_CLIENT_ID)
            .await
            .expect("refresh");
        assert_eq!(
            blockchain.utxo_read_count(),
            before + 1,
            "a refusal should have made the cache stale"
        );
    }

    /// A failed broadcast used to refresh immediately, which spent a request
    /// on an answer the caller could not use. Marking the state stale defers
    /// it to a request that wants it, and to none at all if none comes.
    #[tokio::test]
    async fn cs_431_a_failed_broadcast_does_not_refresh_on_the_spot() {
        use crate::test_support::RejectingBroadcaster;

        let mut config = test_config(&unique_dynamic_config_path());
        config.service.utxo_refresh_period = 3600;
        let blockchain = CountingBlockchain::new(&config).await;
        let service = service_with(
            &config,
            blockchain.clone(),
            RejectingBroadcaster::new(false),
        )
        .await;

        Service::refresh_client_chain_state(&service, TEST_CLIENT_ID)
            .await
            .expect("first refresh");
        let before = blockchain.utxo_read_count();

        let _ = fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID)).await;

        assert_eq!(
            blockchain.utxo_read_count(),
            before,
            "the error path should not have fetched"
        );
    }

    /// The periodic sweep is the other half. A client a request already
    /// refreshed inside the window does not need fetching again on the tick,
    /// so a busy service pays for its quiet clients only.
    #[tokio::test]
    async fn cs_431_the_periodic_sweep_skips_clients_a_request_refreshed() {
        let (service, blockchain) = service_with_freshness(3600).await;
        Service::refresh_client_chain_state(&service, TEST_CLIENT_ID)
            .await
            .expect("a request refreshes it");
        let before = blockchain.utxo_read_count();

        Service::refresh_balances(&service).await;

        assert_eq!(
            blockchain.utxo_read_count(),
            before,
            "the sweep refetched a client that was already current"
        );
    }

    // ---- SR-FUND-031/032, SR-BCHN-015: writes do not queue on the refresh ----

    use crate::test_support::{GatedBlockchain, StubMapiBroadcaster};

    /// Long enough for anything runnable to have run, on the current-thread
    /// runtime a `#[tokio::test]` uses. Only ever used to show something did
    /// NOT finish; nothing waits on it to succeed.
    const SETTLE: std::time::Duration = std::time::Duration::from_millis(50);

    /// A service with `[mapi_lite]` configured whose cache goes stale after
    /// `seconds`, reading through a gate the test holds.
    async fn mapi_lite_service(
        seconds: u64,
    ) -> (Arc<Service>, Arc<GatedBlockchain>, Arc<StubMapiBroadcaster>) {
        let mut config = test_config(&unique_dynamic_config_path());
        config.service.utxo_refresh_period = seconds;
        let blockchain = GatedBlockchain::new(&config).await;
        let broadcaster = StubMapiBroadcaster::new(true);
        let service = service_with(&config, blockchain.clone(), broadcaster.clone()).await;
        (service, blockchain, broadcaster)
    }

    /// Fund one transaction, failing the test rather than hanging it if the
    /// write is held -- which is what a regression here would do.
    async fn fund_without_waiting(service: &Arc<Service>) -> FundingResponse {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fund_single_transaction(service, &sample_fund_request(TEST_CLIENT_ID)),
        )
        .await
        .expect("the write waited on the paced interface")
        .expect("funding should succeed")
    }

    fn fund_in_background(
        service: &Arc<Service>,
    ) -> tokio::task::JoinHandle<Result<FundingResponse, CodedError>> {
        let service = Arc::clone(service);
        tokio::spawn(async move {
            fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID)).await
        })
    }

    /// The point of the change. With mapi-lite taking the broadcast, a write
    /// whose cache has merely aged is funded from it at once; the refresh it
    /// prompts runs behind it, on an interface that is still paced.
    #[tokio::test]
    async fn sr_fund_031_a_mapi_lite_write_does_not_wait_for_a_stale_refresh() {
        // a zero window: the cache is stale for every request
        let (service, blockchain, broadcaster) = mapi_lite_service(0).await;
        let before = blockchain.utxo_read_count();
        blockchain.close();

        let response = fund_without_waiting(&service).await;
        assert_eq!(response.outpoints.len(), 1);
        assert_eq!(broadcaster.broadcast_count(), 1);

        tokio::time::sleep(SETTLE).await;
        assert_eq!(
            blockchain.utxo_read_count(),
            before + 1,
            "the stale cache should still have been refreshed, behind the write"
        );
        blockchain.open();
    }

    /// Without mapi-lite the broadcast goes through the paced interface, so
    /// the write waits on it regardless and the refresh in front of it stays
    /// where it was.
    #[tokio::test]
    async fn sr_fund_031_without_mapi_lite_a_stale_write_still_waits() {
        let mut config = test_config(&unique_dynamic_config_path());
        config.service.utxo_refresh_period = 0;
        let blockchain = GatedBlockchain::new(&config).await;
        let service = Arc::new(Service::new_for_test(&config, blockchain.clone()).await);
        blockchain.close();

        let write = fund_in_background(&service);
        tokio::time::sleep(SETTLE).await;
        assert!(
            !write.is_finished(),
            "the write should wait for the refresh"
        );

        blockchain.open();
        write.await.unwrap().expect("funding should succeed");
        assert_eq!(blockchain.broadcast_count(), 1);
    }

    /// A refused or uncertain broadcast is evidence the cache is wrong, so
    /// the next write waits for a new view even with mapi-lite: funding from
    /// the old one is the conflict the refresh is there to prevent.
    #[tokio::test]
    async fn sr_fund_031_an_invalidated_cache_makes_a_mapi_lite_write_wait() {
        let (service, blockchain, broadcaster) = mapi_lite_service(3600).await;
        Service::invalidate_chain_state(&service, TEST_CLIENT_ID).await;
        let before = blockchain.utxo_read_count();
        blockchain.close();

        let write = fund_in_background(&service);
        tokio::time::sleep(SETTLE).await;
        assert!(
            !write.is_finished(),
            "the write should wait for the refresh"
        );
        assert_eq!(broadcaster.broadcast_count(), 0);

        blockchain.open();
        write.await.unwrap().expect("funding should succeed");
        assert_eq!(blockchain.utxo_read_count(), before + 1);
        assert_eq!(broadcaster.broadcast_count(), 1);
    }

    /// Requests that all find the cache stale share one fetch. Before, each
    /// made its own, and on a paced interface every one past the first
    /// queued for an answer the first had already fetched.
    #[tokio::test]
    async fn sr_fund_032_concurrent_stale_requests_share_one_refresh() {
        let mut config = test_config(&unique_dynamic_config_path());
        config.service.utxo_refresh_period = 3600;
        let blockchain = GatedBlockchain::new(&config).await;
        let service = Arc::new(Service::new_for_test(&config, blockchain.clone()).await);
        Service::invalidate_chain_state(&service, TEST_CLIENT_ID).await;
        let before = blockchain.utxo_read_count();
        blockchain.close();

        let requests: Vec<_> = (0..5)
            .map(|_| {
                let service = Arc::clone(&service);
                tokio::spawn(async move {
                    Service::refresh_client_chain_state_if_stale(&service, TEST_CLIENT_ID).await
                })
            })
            .collect();
        tokio::time::sleep(SETTLE).await;
        assert_eq!(
            blockchain.utxo_read_count(),
            before + 1,
            "one fetch in flight"
        );

        blockchain.open();
        for request in requests {
            request.await.unwrap().expect("refreshed");
        }
        assert_eq!(
            blockchain.utxo_read_count(),
            before + 1,
            "the queued requests should have reused the first one's answer"
        );
    }

    /// The background refresh is single-flighted too: a burst of mapi-lite
    /// writes against a stale cache starts one, not one per write.
    #[tokio::test]
    async fn sr_fund_032_a_burst_of_mapi_lite_writes_starts_one_background_refresh() {
        let (service, blockchain, broadcaster) = mapi_lite_service(0).await;
        let before = blockchain.utxo_read_count();
        blockchain.close();

        for _ in 0..3 {
            fund_without_waiting(&service).await;
        }
        tokio::time::sleep(SETTLE).await;

        assert_eq!(broadcaster.broadcast_count(), 3);
        assert_eq!(blockchain.utxo_read_count(), before + 1);
        blockchain.open();
    }

    /// The periodic sweep leaves a client alone while a refresh of it is in
    /// flight, rather than queueing a second fetch behind it.
    #[tokio::test]
    async fn sr_fund_032_the_periodic_sweep_skips_a_client_being_refreshed() {
        let (service, blockchain, _broadcaster) = mapi_lite_service(0).await;
        let before = blockchain.utxo_read_count();
        blockchain.close();

        fund_without_waiting(&service).await;
        tokio::time::sleep(SETTLE).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Service::refresh_balances(&service),
        )
        .await
        .expect("the sweep waited on the refresh in flight");

        assert_eq!(blockchain.utxo_read_count(), before + 1);
        blockchain.open();
    }

    /// With mapi-lite, nothing a write waits on goes through the paced
    /// interface: not the broadcast (SR-BCHN-009) and, with a usable cache,
    /// not the refresh either. Held here at a closed gate, which is what a
    /// paced interface with a queue in front of it looks like to a caller.
    #[tokio::test]
    async fn sr_bchn_015_a_mapi_lite_write_waits_on_nothing_from_the_paced_interface() {
        let (service, blockchain, broadcaster) = mapi_lite_service(0).await;
        blockchain.close();

        for _ in 0..3 {
            fund_without_waiting(&service).await;
        }

        assert_eq!(broadcaster.broadcast_count(), 3);
        assert_eq!(blockchain.broadcast_count(), 0);
        blockchain.open();
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

    /// The defect in #66, end to end. A funding transaction is handed to the
    /// broadcaster, the outcome is unknown, and the next funding request must
    /// not spend the same input -- which would put a conflicting transaction
    /// on the network for the first one's inputs.
    ///
    /// Both requests are identical, so if the second one selected the same
    /// UTXO it would build a byte-identical transaction and hand over the same
    /// txid. Different txids mean different inputs.
    #[tokio::test]
    async fn sr_fund_012_an_uncertain_broadcast_does_not_leave_its_input_spendable() {
        use crate::test_support::{CountingBlockchain, UncertainBroadcaster};

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = CountingBlockchain::new(&config).await;
        let broadcaster = UncertainBroadcaster::new();
        let service = service_with(&config, blockchain.clone(), broadcaster.clone()).await;

        let request = sample_fund_request(TEST_CLIENT_ID);
        let first = fund_single_transaction(&service, &request)
            .await
            .expect_err("an unknown outcome is not a success");
        assert_eq!(first.code, ErrorCode::BroadcastOutcomeUnknown);

        let second = fund_single_transaction(&service, &request)
            .await
            .expect_err("the second broadcast is equally uncertain");
        assert_eq!(second.code, ErrorCode::BroadcastOutcomeUnknown);

        let handed = broadcaster.handed();
        assert_eq!(handed.len(), 2, "both attempts reached the broadcaster");
        assert_ne!(
            handed[0], handed[1],
            "the second funding transaction spends the same input as the first, which the \
             network can only treat as a conflict"
        );
    }

    /// A broadcast that is *known* to have failed spent nothing, so its input
    /// must stay available -- reserving there would strand funds for no
    /// reason. The distinction is the whole point of the new variant.
    #[tokio::test]
    async fn sr_fund_012_a_failed_broadcast_leaves_its_input_spendable() {
        use crate::test_support::{CountingBlockchain, FailingBroadcaster};

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = CountingBlockchain::new(&config).await;
        // fails from the first call
        let broadcaster = FailingBroadcaster::new(0);
        let service = service_with(&config, blockchain.clone(), broadcaster.clone()).await;

        let request = sample_fund_request(TEST_CLIENT_ID);
        let error = fund_single_transaction(&service, &request)
            .await
            .expect_err("the broadcast failed");
        assert_eq!(error.code, ErrorCode::BroadcastFailed);

        let client = service.client_handle(TEST_CLIENT_ID).await.unwrap();
        assert_eq!(
            client.read().await.reserved_outpoint_count(),
            0,
            "nothing reached the network, so nothing should be reserved"
        );
    }

    /// The reservation has to survive the refresh that runs on the same error
    /// path, or it would be undone the moment it was made.
    #[tokio::test]
    async fn sr_fund_012_the_reservation_survives_the_refresh_on_the_error_path() {
        use crate::test_support::{CountingBlockchain, UncertainBroadcaster};

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = CountingBlockchain::new(&config).await;
        let broadcaster = UncertainBroadcaster::new();
        let service = service_with(&config, blockchain.clone(), broadcaster.clone()).await;

        let _ = fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID)).await;

        let client = service.client_handle(TEST_CLIENT_ID).await.unwrap();
        assert_eq!(client.read().await.reserved_outpoint_count(), 1);

        // and an explicit refresh, as the periodic one does, does not free it
        Service::refresh_client_chain_state(&service, TEST_CLIENT_ID)
            .await
            .expect("refresh succeeds");
        assert_eq!(client.read().await.reserved_outpoint_count(), 1);
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

    /// The quote wins over the configured rate, and reaches the clients that
    /// actually cost transactions -- not just the service's own copy.
    #[tokio::test]
    async fn cs_451_the_mapi_fee_quote_sets_the_rate() {
        use crate::test_support::StubMapiBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service =
            service_with(&config, blockchain, StubMapiBroadcaster::quoting(Some(250))).await;

        assert_eq!(
            service.fee_satoshis_per_kb().await,
            config.fees.satoshis_per_kb,
            "starts at the configured rate"
        );

        service.refresh_fee_rate().await;

        assert_eq!(service.fee_satoshis_per_kb().await, 250);
        let client = service.client_handle(TEST_CLIENT_ID).await.expect("client");
        assert_eq!(
            client.read().await.fee_satoshis_per_kb(),
            250,
            "the rate has to reach the client, which is what costs a transaction"
        );
    }

    /// A quote that cannot be had is not a reason to stop funding, nor to fall
    /// to some other number: the rate in force stays in force.
    #[tokio::test]
    async fn cs_451_a_failed_quote_leaves_the_rate_alone() {
        use crate::test_support::StubMapiBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let broadcaster = StubMapiBroadcaster::quoting(None);
        let handed: Arc<dyn TxBroadcaster> = broadcaster.clone();
        let service = service_with(&config, blockchain, handed).await;

        service.refresh_fee_rate().await;

        assert_eq!(broadcaster.quote_count(), 1, "it did ask");
        assert_eq!(
            service.fee_satoshis_per_kb().await,
            config.fees.satoshis_per_kb,
            "and kept the configured rate when there was no answer"
        );
    }

    /// Turning the quote off means the configured rate is authoritative, and
    /// mapi-lite is not asked at all.
    #[tokio::test]
    async fn cs_451_the_quote_can_be_turned_off() {
        use crate::test_support::StubMapiBroadcaster;

        let mut config = test_config(&unique_dynamic_config_path());
        config.fees.use_mapi_fee_quote = false;
        config.fees.satoshis_per_kb = 175;

        let blockchain = test_blockchain_interface(&config).await;
        let broadcaster = StubMapiBroadcaster::quoting(Some(250));
        let handed: Arc<dyn TxBroadcaster> = broadcaster.clone();
        let service = service_with(&config, blockchain, handed).await;

        service.refresh_fee_rate().await;

        assert_eq!(broadcaster.quote_count(), 0, "not even asked");
        assert_eq!(service.fee_satoshis_per_kb().await, 175);
        let client = service.client_handle(TEST_CLIENT_ID).await.expect("client");
        assert_eq!(client.read().await.fee_satoshis_per_kb(), 175);
    }

    /// A client added after a quote has moved the rate must cost its
    /// transactions the same as one that was here all along.
    #[tokio::test]
    async fn cs_451_a_client_added_later_gets_the_rate_in_force() {
        use crate::test_support::StubMapiBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;
        let service =
            service_with(&config, blockchain, StubMapiBroadcaster::quoting(Some(250))).await;
        service.refresh_fee_rate().await;

        service
            .add_client(&ClientConfig {
                client_id: "added-later".to_string(),
                wif_key: crate::test_support::TEST_WIF.to_string(),
                api_key: None,
            })
            .await
            .expect("added");

        let client = service.client_handle("added-later").await.expect("client");
        assert_eq!(
            client.read().await.fee_satoshis_per_kb(),
            250,
            "not the config file's rate, which the quote has superseded"
        );
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

    // ---- CS-465: what was in flight survives a restart ----

    /// The input a funding transaction spent.
    fn spent_input(response: &FundingResponse) -> (String, u32) {
        let input = &response.txs[0].inputs[0].prev_output;
        (input.hash.encode(), input.index)
    }

    /// A chain holding exactly `utxo` for the test client, that never changes.
    ///
    /// Never changing is the point. Straight after a broadcast the read
    /// interface has usually not seen the transaction yet, so it goes on
    /// reporting the input as unspent and says nothing of the change -- and
    /// that is the chain a restart comes back to.
    async fn chain_holding(
        config: &Config,
        utxo: Vec<chain_gang::interface::UtxoEntry>,
    ) -> Arc<dyn BlockchainInterface + Send + Sync> {
        let mut interface = chain_gang::interface::TestInterface::new();
        interface.set_network(&config.get_network().unwrap());
        interface.set_utxo(TEST_ADDRESS, &utxo).await;
        interface.set_height(1_517_571).await;
        Arc::new(interface)
    }

    fn confirmed_utxo(value: i64) -> chain_gang::interface::UtxoEntry {
        chain_gang::interface::UtxoEntry {
            height: 1_514_933,
            tx_pos: 0,
            tx_hash: "f67272e5c1408ecbeb8da543437c125ee1a17110317d44d13eafe31b771b795e".to_string(),
            value,
        }
    }

    /// The ticket's scenario. Fund, restart, fund again against a chain that
    /// has not caught up. Without the state file the second run spends the
    /// same input, rebuilds the same transaction byte for byte -- signing is
    /// deterministic -- and hands the same outpoint to a second caller.
    #[tokio::test]
    async fn cs_465_a_restart_does_not_hand_out_an_input_already_spent() {
        let config = test_config(&unique_dynamic_config_path());
        let request = sample_fund_request(TEST_CLIENT_ID);

        let before = Arc::new(
            Service::new_for_test(&config, test_blockchain_interface(&config).await).await,
        );
        let first = fund_single_transaction(&before, &request)
            .await
            .expect("funds");
        drop(before);

        let after = Arc::new(
            Service::new_for_test(&config, test_blockchain_interface(&config).await).await,
        );
        let second = fund_single_transaction(&after, &request)
            .await
            .expect("funds");

        assert_ne!(
            spent_input(&first),
            spent_input(&second),
            "the restarted service spent the input its previous run had already spent"
        );
        assert_ne!(first.txs[0].hash(), second.txs[0].hash());
    }

    /// And it carries on from where it stopped: the change its last
    /// transaction created is spendable after the restart, though the chain
    /// has not reported it yet. That is the chain the ticket's first run built,
    /// continued rather than restarted.
    #[tokio::test]
    async fn cs_465_a_restart_carries_on_from_its_own_change() {
        let config = test_config(&unique_dynamic_config_path());
        let mut request = sample_fund_request(TEST_CLIENT_ID);
        request.satoshi = 10;

        let before = Arc::new(
            Service::new_for_test(
                &config,
                chain_holding(&config, vec![confirmed_utxo(5_000)]).await,
            )
            .await,
        );
        let first = fund_single_transaction(&before, &request)
            .await
            .expect("funds");
        drop(before);

        let after = Arc::new(
            Service::new_for_test(
                &config,
                chain_holding(&config, vec![confirmed_utxo(5_000)]).await,
            )
            .await,
        );
        let second = fund_single_transaction(&after, &request)
            .await
            .expect("the change from before the restart is still spendable");

        assert_eq!(
            spent_input(&second),
            (first.txs[0].hash().encode(), 0),
            "the second transaction spends the first one's change, output 0"
        );
    }

    /// A broadcast of unknown outcome reserves its inputs too, and that has to
    /// survive a restart just as a successful one does -- it is the same risk.
    #[tokio::test]
    async fn cs_465_an_uncertain_broadcast_is_remembered_across_a_restart() {
        use crate::test_support::UncertainBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let request = sample_fund_request(TEST_CLIENT_ID);

        let broadcaster = UncertainBroadcaster::new();
        let before = service_with(
            &config,
            test_blockchain_interface(&config).await,
            broadcaster.clone(),
        )
        .await;
        let _ = fund_single_transaction(&before, &request).await;
        drop(before);

        let after = service_with(
            &config,
            test_blockchain_interface(&config).await,
            broadcaster.clone(),
        )
        .await;
        let _ = fund_single_transaction(&after, &request).await;

        let handed = broadcaster.handed();
        assert_eq!(handed.len(), 2);
        assert_ne!(
            handed[0], handed[1],
            "after the restart the service rebuilt the transaction whose outcome it did not know"
        );
    }

    /// A state file that cannot be read is survived: the service starts, as it
    /// would have with no file at all, rather than refusing to.
    #[tokio::test]
    async fn cs_465_an_unreadable_state_file_does_not_stop_startup() {
        let config = test_config(&unique_dynamic_config_path());
        std::fs::write(config.inflight_state_path(), "{ not json").unwrap();

        let service = Arc::new(
            Service::new_for_test(&config, test_blockchain_interface(&config).await).await,
        );
        let client = service.client_handle(TEST_CLIENT_ID).await.expect("client");
        assert_eq!(client.read().await.reserved_outpoint_count(), 0);
        fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect("and funds as before");
    }

    // ---- CS-495: commits share in-flight saves ----

    fn inflight_writes(service: &Service) -> u64 {
        service
            .inflight_writes
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn reservations_on_disk(config: &Config) -> usize {
        load_inflight_file(&config.inflight_state_path())
            .get(TEST_CLIENT_ID)
            .map_or(0, |state| state.reserved.len())
    }

    /// The ticket's point. Commits that finish while a write is in progress
    /// queue for the next one, and that one write covers all of them: three
    /// fundings, one write, and all three of their spends on disk before any
    /// of them is answered.
    #[tokio::test]
    async fn cs_495_commits_waiting_on_one_write_share_it() {
        let config = test_config(&unique_dynamic_config_path());
        let service = Arc::new(
            Service::new_for_test(&config, test_blockchain_interface(&config).await).await,
        );
        let before = inflight_writes(&service);

        // a write in progress
        let writing = service.inflight_save.lock().await;
        let requests: Vec<_> = (0..3)
            .map(|_| {
                let service = Arc::clone(&service);
                tokio::spawn(async move {
                    fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID)).await
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            requests.iter().all(|request| !request.is_finished()),
            "a funding was answered before its spend was written"
        );

        drop(writing);
        for request in requests {
            request.await.unwrap().expect("funds");
        }
        assert_eq!(
            inflight_writes(&service) - before,
            1,
            "one write for three commits"
        );
        assert_eq!(
            reservations_on_disk(&config),
            3,
            "and it holds all three spends"
        );
    }

    /// Sharing is only with commits already waiting. A commit that arrives
    /// after a write has been taken is not in it, so it gets a write of its
    /// own rather than being answered on the strength of an older one.
    #[tokio::test]
    async fn cs_495_a_commit_after_a_write_gets_the_next_one() {
        let config = test_config(&unique_dynamic_config_path());
        let service = Arc::new(
            Service::new_for_test(&config, test_blockchain_interface(&config).await).await,
        );
        let before = inflight_writes(&service);

        fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect("funds");
        assert_eq!(reservations_on_disk(&config), 1);
        fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect("funds");
        assert_eq!(reservations_on_disk(&config), 2);
        assert_eq!(inflight_writes(&service) - before, 2);
    }

    /// A write that fails covers nothing. Commits queued behind it are not
    /// answered on the strength of it: the next one tries again. And once the
    /// file can be written it holds every spend, including those whose own
    /// write failed.
    #[tokio::test]
    async fn cs_495_a_failed_write_is_not_taken_as_covering_anything() {
        let config = test_config(&unique_dynamic_config_path());
        let service = Arc::new(
            Service::new_for_test(&config, test_blockchain_interface(&config).await).await,
        );
        // nothing can be renamed over a directory that has something in it
        let path = config.inflight_state_path();
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("occupied"), "").unwrap();
        let before = inflight_writes(&service);

        // two commits queued behind a write in progress, as in the sharing case
        let writing = service.inflight_save.lock().await;
        let requests: Vec<_> = (0..2)
            .map(|_| {
                let service = Arc::clone(&service);
                tokio::spawn(async move {
                    fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID)).await
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(writing);
        for request in requests {
            request
                .await
                .unwrap()
                .expect("funds, though the state cannot be saved");
        }
        assert_eq!(
            inflight_writes(&service) - before,
            2,
            "the second commit should have tried again after the first write failed"
        );

        std::fs::remove_dir_all(&path).unwrap();
        fund_single_transaction(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect("funds");
        assert_eq!(
            reservations_on_disk(&config),
            3,
            "all three spends, once it could"
        );
    }

    // ---- CS-473: concurrent requests for one client ----

    /// The ticket's load. Requests for one client overlap -- each spends as
    /// long between planning and committing as its broadcast takes -- and no
    /// two of the transactions they broadcast may spend the same input. One
    /// of them would be refused by the network as a conflict.
    #[tokio::test]
    async fn cs_473_concurrent_requests_for_one_client_never_share_an_input() {
        use crate::test_support::SlowRecordingBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let broadcaster = SlowRecordingBroadcaster::new(Duration::from_millis(50));
        let service = service_with(
            &config,
            test_blockchain_interface(&config).await,
            broadcaster.clone(),
        )
        .await;
        let mut request = sample_fund_request(TEST_CLIENT_ID);
        request.satoshi = 10;

        let calls: Vec<_> = (0..8)
            .map(|_| {
                let service = Arc::clone(&service);
                let request = request.clone();
                tokio::spawn(async move { Service::execute_funding(&service, &request).await })
            })
            .collect();
        for call in calls {
            let _ = call.await.expect("task");
        }

        let spent = broadcaster.spent_inputs();
        let mut seen = std::collections::HashSet::new();
        let reused: Vec<_> = spent.iter().filter(|input| !seen.insert(*input)).collect();
        assert!(
            reused.is_empty(),
            "{} of {} inputs were spent by more than one transaction: {reused:?}",
            reused.len(),
            spent.len()
        );
    }

    /// A broadcast the upstream definitely refused spent nothing, so the input
    /// it claimed goes straight back: not reserved, and spendable by the next
    /// request without waiting for a refresh.
    #[tokio::test]
    async fn cs_473_a_refused_broadcast_gives_its_input_back() {
        use crate::test_support::RejectingBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let service = service_with(
            &config,
            test_blockchain_interface(&config).await,
            RejectingBroadcaster::new(false),
        )
        .await;
        let client = service.client_handle(TEST_CLIENT_ID).await.expect("client");
        let held_before = client.read().await.inflight_state();

        let refused = Service::execute_funding(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect_err("the upstream refuses");
        assert_eq!(refused.code, ErrorCode::BroadcastRejected);

        assert_eq!(
            client.read().await.reserved_outpoint_count(),
            0,
            "nothing left claimed"
        );
        assert_eq!(client.read().await.inflight_state(), held_before);
    }

    /// When one UTXO is all the client has, a second request arriving while
    /// the first is broadcasting finds nothing to spend. It is told so, with
    /// a coded error, instead of building a conflicting transaction -- and
    /// only one transaction ever reaches the network.
    #[tokio::test]
    async fn cs_473_contention_for_one_utxo_is_refused_rather_than_conflicted() {
        use crate::test_support::SlowRecordingBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let broadcaster = SlowRecordingBroadcaster::new(Duration::from_millis(100));
        let service = service_with(
            &config,
            chain_holding(&config, vec![confirmed_utxo(50_000)]).await,
            broadcaster.clone(),
        )
        .await;
        let mut request = sample_fund_request(TEST_CLIENT_ID);
        request.satoshi = 10;

        let first = {
            let (service, request) = (Arc::clone(&service), request.clone());
            tokio::spawn(async move { Service::execute_funding(&service, &request).await })
        };
        // The first has claimed the only UTXO and is still broadcasting
        tokio::time::sleep(Duration::from_millis(30)).await;
        let second = Service::execute_funding(&service, &request).await;

        assert!(first.await.expect("task").is_ok(), "the first funds");
        let refused = second.expect_err("the second has nothing to spend");
        assert_ne!(
            refused.code,
            ErrorCode::Internal,
            "an honest code, not a 500: {refused:?}"
        );
        assert_ne!(
            refused.code,
            ErrorCode::BroadcastRejected,
            "refused before the network, not by it"
        );
        assert_eq!(
            broadcaster.spent_inputs().len(),
            1,
            "one transaction reached the network"
        );
    }

    // ---- CS-474: the service's contract, stated from the caller's side ----

    /// Concurrent identical requests -- the ticket's load -- and no outpoint
    /// in any successful response may appear in another. Checked on what the
    /// callers were told, not on what was broadcast: a duplicate used to be a
    /// 200 to every one of them, with nothing logged.
    #[tokio::test]
    async fn cs_474_concurrent_identical_requests_are_never_given_the_same_outpoint() {
        use crate::test_support::SlowRecordingBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let broadcaster = SlowRecordingBroadcaster::new(Duration::from_millis(50));
        let service = service_with(
            &config,
            test_blockchain_interface(&config).await,
            broadcaster.clone(),
        )
        .await;
        let mut request = sample_fund_request(TEST_CLIENT_ID);
        request.satoshi = 10;

        let calls: Vec<_> = (0..10)
            .map(|_| {
                let (service, request) = (Arc::clone(&service), request.clone());
                tokio::spawn(async move { Service::execute_funding(&service, &request).await })
            })
            .collect();
        let mut handed = std::collections::HashMap::new();
        for call in calls {
            if let Ok(response) = call.await.expect("task") {
                for outpoint in &response.outpoints {
                    *handed
                        .entry((outpoint.hash.encode(), outpoint.index))
                        .or_insert(0) += 1;
                }
            }
        }
        let shared: Vec<_> = handed.iter().filter(|(_, n)| **n > 1).collect();
        assert!(!handed.is_empty(), "something was funded");
        assert!(
            shared.is_empty(),
            "outpoints given to more than one caller: {shared:?}"
        );
    }

    // ---- CS-475: a refusal under load says the funds are in flight ----

    /// The ticket's load against a wallet of three UTXOs. Every request that
    /// finds them all claimed is refused with `funds_in_flight` -- not with
    /// the codes that tell an operator to top up or consolidate -- and none
    /// of the refusals is anything else.
    #[tokio::test]
    async fn cs_475_contention_is_reported_as_funds_in_flight() {
        use crate::test_support::SlowRecordingBroadcaster;

        let config = test_config(&unique_dynamic_config_path());
        let broadcaster = SlowRecordingBroadcaster::new(Duration::from_millis(100));
        let chain = vec![
            confirmed_utxo(50_000),
            chain_gang::interface::UtxoEntry {
                tx_pos: 1,
                ..confirmed_utxo(50_000)
            },
            chain_gang::interface::UtxoEntry {
                tx_pos: 2,
                ..confirmed_utxo(50_000)
            },
        ];
        let service = service_with(
            &config,
            chain_holding(&config, chain).await,
            broadcaster.clone(),
        )
        .await;
        let mut request = sample_fund_request(TEST_CLIENT_ID);
        request.satoshi = 10;

        let calls: Vec<_> = (0..8)
            .map(|_| {
                let (service, request) = (Arc::clone(&service), request.clone());
                tokio::spawn(async move { Service::execute_funding(&service, &request).await })
            })
            .collect();
        let mut refusals = Vec::new();
        for call in calls {
            if let Err(error) = call.await.expect("task") {
                refusals.push(error.code);
            }
        }
        assert!(!refusals.is_empty(), "the load outran the wallet");
        assert!(
            refusals
                .iter()
                .all(|code| *code == ErrorCode::FundsInFlight),
            "{refusals:?}"
        );
    }

    // ---- CS-482: every broadcast's outcome is logged with its txid ----

    /// The lines this test's thread logged that name `txid`.
    fn lines_naming(txid: &str) -> Vec<(log::Level, String)> {
        crate::test_support::logged_on_this_thread()
            .into_iter()
            .filter(|(_, line)| line.contains(&format!("funding tx {txid}")))
            .collect()
    }

    /// The txid of every attempt this test's thread logged, in order.
    fn attempted_txids() -> Vec<String> {
        crate::test_support::logged_on_this_thread()
            .into_iter()
            .filter_map(|(_, line)| {
                line.strip_prefix("broadcasting funding tx ")
                    .map(|rest| rest[..64].to_string())
            })
            .collect()
    }

    /// A success is logged as one: the attempt, naming the client, the
    /// outpoint count and the broadcaster, then an acceptance. It used to log
    /// nothing at all, so success could only be inferred from a failure line
    /// not appearing.
    #[tokio::test]
    async fn cs_482_an_accepted_broadcast_is_logged_with_its_txid() {
        crate::test_support::capture_logs();
        let config = test_config(&unique_dynamic_config_path());
        let service = service_with(
            &config,
            test_blockchain_interface(&config).await,
            crate::test_support::CountingBroadcaster::new(),
        )
        .await;

        let funded = Service::execute_funding(&service, &sample_fund_request(TEST_CLIENT_ID))
            .await
            .expect("funds");
        let txid = funded.txs[0].hash().encode();

        let lines = lines_naming(&txid);
        let levels: Vec<_> = lines.iter().map(|(level, _)| *level).collect();
        assert_eq!(
            levels,
            vec![log::Level::Info, log::Level::Debug, log::Level::Info],
            "{lines:?}"
        );
        assert_eq!(
            lines[0].1,
            format!(
                "broadcasting funding tx {txid} (client_id={TEST_CLIENT_ID}, outpoints=1, \
                 broadcaster=counting)"
            )
        );
        assert!(
            lines[1].1.starts_with(&format!("funding tx {txid} hex=")),
            "{lines:?}"
        );
        assert_eq!(
            lines[2].1,
            format!("broadcast accepted: funding tx {txid} (broadcaster=counting)")
        );
    }

    /// A failure names the transaction, its class and the upstream's reason.
    /// The class is the one the caller's error code says, so the log and the
    /// response agree -- including a refusal the upstream called retryable,
    /// which the caller is told is `broadcast_failed`.
    #[tokio::test]
    async fn cs_482_a_failed_broadcast_is_logged_with_its_txid_and_class() {
        use crate::test_support::{RejectingBroadcaster, UncertainBroadcaster};

        crate::test_support::capture_logs();
        let cases: Vec<(Arc<dyn TxBroadcaster>, ErrorCode, &str, &str)> = vec![
            (
                RejectingBroadcaster::new(false),
                ErrorCode::BroadcastRejected,
                "class=rejected, broadcaster=rejecting",
                "rejected: txn-mempool-conflict (retryable: false)",
            ),
            (
                RejectingBroadcaster::new(true),
                ErrorCode::BroadcastFailed,
                "class=failed, broadcaster=rejecting",
                "rejected: txn-mempool-conflict (retryable: true)",
            ),
            (
                UncertainBroadcaster::new(),
                ErrorCode::BroadcastOutcomeUnknown,
                "class=indeterminate, broadcaster=uncertain",
                "outcome unknown: simulated submit deadline expiry",
            ),
        ];
        for (broadcaster, code, class, reason) in cases {
            // Signing is deterministic, so every case builds the same
            // transaction: read only what this case logged.
            let already = crate::test_support::logged_on_this_thread().len();
            let config = test_config(&unique_dynamic_config_path());
            let service = service_with(
                &config,
                test_blockchain_interface(&config).await,
                broadcaster,
            )
            .await;

            let error = Service::execute_funding(&service, &sample_fund_request(TEST_CLIENT_ID))
                .await
                .expect_err("the broadcast does not succeed");
            assert_eq!(error.code, code);

            let txid = attempted_txids().pop().expect("the attempt was logged");
            let this_case: Vec<_> = crate::test_support::logged_on_this_thread()
                .split_off(already)
                .into_iter()
                .filter(|(_, line)| line.contains(&format!("funding tx {txid}")))
                .collect();
            let failure: Vec<_> = this_case
                .iter()
                .filter(|(level, _)| *level == log::Level::Warn)
                .cloned()
                .collect();
            assert_eq!(
                failure,
                vec![(
                    log::Level::Warn,
                    format!("broadcast failed: funding tx {txid} ({class}): {reason}")
                )],
                "{code:?}"
            );
            assert!(
                !this_case
                    .iter()
                    .any(|(_, line)| line.starts_with("broadcast accepted")),
                "a failure is not also logged as accepted"
            );
        }
    }

    /// Separate transactions get a pair of lines each, so a partial broadcast
    /// reads as accepted lines followed by one failure -- each naming its own
    /// transaction.
    #[tokio::test]
    async fn cs_482_a_partial_broadcast_logs_each_transaction() {
        crate::test_support::capture_logs();
        let config = test_config(&unique_dynamic_config_path());
        let service = service_with(
            &config,
            test_blockchain_interface(&config).await,
            crate::test_support::FailingBroadcaster::new(1),
        )
        .await;
        let request = FundRequest {
            no_of_outpoints: 2,
            multiple_tx: true,
            ..sample_fund_request(TEST_CLIENT_ID)
        };

        let _ = Service::fund_with_multiple_transactions(&service, &request)
            .await
            .expect_err("the second transaction fails");

        let txids = attempted_txids();
        assert_eq!(txids.len(), 2, "{txids:?}");
        assert_ne!(txids[0], txids[1]);
        assert!(lines_naming(&txids[0])
            .iter()
            .any(|(level, line)| *level == log::Level::Info
                && line.starts_with("broadcast accepted")));
        assert!(lines_naming(&txids[1])
            .iter()
            .any(|(level, line)| *level == log::Level::Warn
                && line.starts_with(&format!("broadcast failed: funding tx {}", txids[1]))
                && line.contains("class=failed, broadcaster=failing")
                && line.ends_with("upstream error: simulated broadcast failure")));
    }
}
