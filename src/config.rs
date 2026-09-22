use std::{env, net::Ipv4Addr};

use chain_gang::network::Network;
use log::debug;
use serde::{Deserialize, Serialize};

use crate::secrets::{resolve_secret, warn_plaintext_secrets};

/// Blockchain Interface Configuration
#[derive(Debug, Deserialize, Clone)]
pub struct BlockchainInterfaceConfig {
    pub interface_type: String,
    pub network_type: String,
    /// Endpoint for the interfaces that need one: the UaaS base URL, or the
    /// node's JSON-RPC host and port for `rpc` (a bare `host:port` is taken
    /// as `http://`).
    pub url: Option<String>,
    /// JSON-RPC username, for `interface_type = "rpc"`.
    #[serde(default)]
    pub rpc_user: Option<String>,
    /// JSON-RPC password, for `interface_type = "rpc"`. Takes an
    /// `env:VAR_NAME` reference, and is overridden by `FS_RPC_PASSWORD`.
    #[serde(default)]
    pub rpc_password: Option<String>,
    /// Whether to ask the node to watch each client's funding address at
    /// startup, and when a client is added at runtime.
    ///
    /// On by default, because a node reports zero for an address its wallet
    /// does not track -- with no error -- so without the import the service
    /// looks broken. Turn it off if you manage the node's wallet yourself, or
    /// if it is a descriptor wallet, where `importaddress` is refused.
    #[serde(default = "default_rpc_import_addresses")]
    pub rpc_import_addresses: bool,
    /// Ceiling on outbound requests per second to the blockchain interface.
    ///
    /// Left unset it follows the interface: `woc` talks to a public API that
    /// documents "up to 3 requests/sec is free" and answers 429 above it, so
    /// it is limited by default; `rpc` and `uaas` are the operator's own
    /// servers and are not. Set it explicitly to override either way, or to
    /// `0` to turn the limit off.
    #[serde(default)]
    pub max_requests_per_second: Option<u32>,
}

fn default_rpc_import_addresses() -> bool {
    true
}

/// WhatsOnChain's documented free-tier allowance, and so the default for
/// `interface_type = "woc"`. Exceeding it earns a 429, and sustained
/// violation earns a ban, which no retry recovers from.
pub const WOC_REQUESTS_PER_SECOND: u32 = 3;

impl Default for BlockchainInterfaceConfig {
    fn default() -> Self {
        BlockchainInterfaceConfig {
            interface_type: String::new(),
            network_type: String::new(),
            url: None,
            rpc_user: None,
            rpc_password: None,
            rpc_import_addresses: default_rpc_import_addresses(),
            max_requests_per_second: None,
        }
    }
}

impl BlockchainInterfaceConfig {
    /// Outbound requests per second to allow, or `None` for no limit.
    ///
    /// An explicit setting always wins, including an explicit `0` meaning
    /// unlimited. Otherwise only the interfaces that call somebody else's
    /// server are limited.
    pub fn outbound_rate_limit(&self) -> Option<u32> {
        match self.max_requests_per_second {
            Some(0) => None,
            Some(limit) => Some(limit),
            None if self.interface_type == "woc" => Some(WOC_REQUESTS_PER_SECOND),
            None => None,
        }
    }

    /// Reject a configuration the chosen interface cannot work with, at
    /// startup rather than on the first request.
    pub fn validate(&self) -> Result<(), String> {
        match self.interface_type.as_str() {
            "uaas" => {
                if self.url.as_deref().unwrap_or("").is_empty() {
                    return Err(
                        "blockchain_interface.url is required for interface_type = \"uaas\""
                            .to_string(),
                    );
                }
            }
            "rpc" => {
                if self.url.as_deref().unwrap_or("").is_empty() {
                    return Err("blockchain_interface.url is required for interface_type = \"rpc\" (the node's JSON-RPC host and port)".to_string());
                }
                if self.rpc_user.as_deref().unwrap_or("").is_empty() {
                    return Err(
                        "blockchain_interface.rpc_user is required for interface_type = \"rpc\""
                            .to_string(),
                    );
                }
                if self.rpc_password.as_deref().unwrap_or("").is_empty() {
                    return Err(
                        "blockchain_interface.rpc_password is required for interface_type = \"rpc\""
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Client Configuration
#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct ClientConfig {
    pub client_id: String,
    pub wif_key: String,
    /// When set, required for this client's API endpoints.
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct LoggingConfig {
    pub level: String,
}

/// OpenTelemetry trace export configuration.
#[derive(Debug, Default, Deserialize, Clone)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub service_name: Option<String>,
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct ServiceConfig {
    pub utxo_refresh_period: u64,
    /// How old the cached chain state may be before a request refreshes it,
    /// in seconds.
    ///
    /// Defaults to `utxo_refresh_period`, so a request reuses whatever the
    /// periodic refresh last fetched and the steady-state cost is one request
    /// per client per period however much traffic arrives. Lower it to narrow
    /// the window in which the service can build on a view of the chain that
    /// something outside the service has moved on from; `0` refreshes on every
    /// request, which is what the service did before.
    #[serde(default)]
    pub chain_state_max_age_seconds: Option<u64>,
}

impl ServiceConfig {
    /// How old cached chain state may be before a request refreshes it.
    pub fn chain_state_max_age(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.chain_state_max_age_seconds
                .unwrap_or(self.utxo_refresh_period),
        )
    }
}

/// Retention policy for `POST /fund` idempotency records.
///
/// Records are held in memory only, so they do not survive a restart -- a
/// retry that spans one can still produce a second funding transaction.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct IdempotencyConfig {
    /// How long a completed record stays replayable, in seconds.
    #[serde(default = "default_idempotency_ttl_seconds")]
    pub ttl_seconds: u64,
    /// Upper bound on retained records, so a client cannot grow the store
    /// without limit by sending a fresh `idempotency_key` every time.
    #[serde(default = "default_idempotency_max_entries")]
    pub max_entries: usize,
}

/// The fee rate the service pays on the funding transactions it builds.
///
/// Before CS-451 this was hardcoded as `((tx_bytes / 1000) * 500) + 750`,
/// which is a step function rather than a rate: it charged 750 satoshi for any
/// transaction under a kilobyte, and jumped by 500 at each kilobyte after.
/// A 250-byte funding transaction -- the ordinary shape, one input and two
/// outputs -- therefore paid 750 satoshi, an effective 3000 sat/KB. The rate
/// is now stated directly, so what the service pays is a number an operator
/// can read and change rather than an artefact of an expression.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct FeesConfig {
    /// Satoshis per kilobyte of transaction. The fee for a transaction is
    /// `ceil(tx_bytes * satoshis_per_kb / 1000)`, rounded up so that rounding
    /// never underpays and risks a rejection.
    #[serde(default = "default_satoshis_per_kb")]
    pub satoshis_per_kb: u64,
    /// When `[mapi_lite]` is configured, take the rate from its `feeQuote`
    /// instead of `satoshis_per_kb`, so one server sets the rate for every
    /// service that broadcasts through it.
    ///
    /// `satoshis_per_kb` stays the fallback: it is used before the first quote
    /// arrives, and whenever a refresh fails. Without `[mapi_lite]` this has
    /// no effect, because there is nothing to ask.
    #[serde(default = "default_use_mapi_fee_quote")]
    pub use_mapi_fee_quote: bool,
    /// Change below this many satoshis is not worth an output (CS-452).
    ///
    /// A change output this small costs more to spend later than it is worth,
    /// and an output of one satoshi -- which funding the advertised maximum
    /// used to produce -- is simply stranded. Rather than create one, the
    /// service leaves the change out and the amount goes to the miner as extra
    /// fee, so funding the maximum spends the wallet down cleanly.
    ///
    /// This bounds what can be given away: at most `dust_threshold_satoshis`
    /// minus one, and only on a transaction that would otherwise have created
    /// an output nobody would spend.
    #[serde(default = "default_dust_threshold_satoshis")]
    pub dust_threshold_satoshis: u64,
}

impl Default for FeesConfig {
    fn default() -> Self {
        FeesConfig {
            satoshis_per_kb: default_satoshis_per_kb(),
            use_mapi_fee_quote: default_use_mapi_fee_quote(),
            dust_threshold_satoshis: default_dust_threshold_satoshis(),
        }
    }
}

impl FeesConfig {
    /// A rate of zero would build transactions no miner accepts, and the
    /// service would only find out at broadcast.
    pub fn validate(&self) -> Result<(), String> {
        if self.satoshis_per_kb == 0 {
            return Err("fees.satoshis_per_kb must be greater than 0: a \
                        transaction paying no fee is not relayed."
                .to_string());
        }
        Ok(())
    }
}

/// Satoshis per kilobyte when nothing is configured.
///
/// Deliberately far below what the superseded expression charged. See
/// [`FeesConfig`] and docs/Configuration.md: an upgrade without a `[fees]`
/// section pays less than it used to, which is the point of CS-451, but it is
/// a behaviour change and not only a refactor.
pub const DEFAULT_SATOSHIS_PER_KB: u64 = 100;

fn default_satoshis_per_kb() -> u64 {
    DEFAULT_SATOSHIS_PER_KB
}

fn default_use_mapi_fee_quote() -> bool {
    true
}

/// Satoshis below which change is folded into the fee rather than paid out.
///
/// The P2PKH dust figure at the relay fee BSV nodes actually apply, rather
/// than Bitcoin Core's better-known 546, which was derived from a relay fee an
/// order of magnitude higher and would give away far more.
pub const DEFAULT_DUST_THRESHOLD_SATOSHIS: u64 = 135;

fn default_dust_threshold_satoshis() -> u64 {
    DEFAULT_DUST_THRESHOLD_SATOSHIS
}

impl Default for IdempotencyConfig {
    fn default() -> Self {
        IdempotencyConfig {
            ttl_seconds: default_idempotency_ttl_seconds(),
            max_entries: default_idempotency_max_entries(),
        }
    }
}

fn default_idempotency_ttl_seconds() -> u64 {
    600
}

fn default_idempotency_max_entries() -> usize {
    10_000
}

impl IdempotencyConfig {
    pub fn ttl(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.ttl_seconds)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.ttl_seconds == 0 {
            return Err("idempotency.ttl_seconds must be greater than zero".to_string());
        }
        if self.max_entries == 0 {
            return Err("idempotency.max_entries must be greater than zero".to_string());
        }
        Ok(())
    }
}

/// Optional mapi-lite integration.
///
/// When this section is present, funding transactions are broadcast through
/// the named mapi-lite server instead of through the `[blockchain_interface]`;
/// chain reads (balances, UTXOs) are unaffected. The two settings that select
/// and authenticate the server mirror the ones teranode-event-rs uses for the
/// same integration (`mapi_base_url`, `auth_token`); the rest bound how long a
/// funding call may wait on mapi-lite.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct MapiLiteConfig {
    /// Base URL of the mapi-lite server, e.g. `http://127.0.0.1:8080`.
    /// Surrounding whitespace and a trailing `/` are tolerated; read it
    /// through [`MapiLiteConfig::base_url`] rather than touching the field,
    /// so what was validated is what gets requested.
    pub base_url: String,
    /// Sent verbatim as the `Authorization` header on every request, so it
    /// must include the scheme: `"Bearer <secret>"`. Takes an `env:VAR_NAME`
    /// reference, and is overridden by `FS_MAPI_LITE_AUTH_TOKEN`.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Per-request timeout for transaction submits, in seconds.
    #[serde(default = "default_mapi_lite_timeout_seconds")]
    pub timeout_seconds: u64,
    /// Timeout for the mapi-lite probe behind `GET /ready`, in seconds. Must
    /// be under three seconds, and is rejected at startup otherwise: a probe
    /// slower than the few seconds a readiness probe allows
    /// would mark the container unhealthy even while mapi-lite is fine.
    #[serde(default = "default_mapi_lite_health_timeout_seconds")]
    pub health_timeout_seconds: u64,
    /// How many times a submit is retried after a transient failure (an HTTP
    /// 5xx or a transport error) before `/fund` reports `broadcast_failed`.
    /// Resubmitting is safe: mapi-lite answers a known transaction with
    /// success.
    #[serde(default = "default_mapi_lite_max_retries")]
    pub max_retries: u32,
    /// Upper bound on a whole submit -- every attempt and every back-off
    /// between them -- in seconds. Without it the worst case is
    /// `timeout_seconds * (max_retries + 1)` plus back-off, which against a
    /// wedged mapi-lite holds a `/fund` request, and the worker serving it,
    /// open for minutes. Must be at least `timeout_seconds`.
    #[serde(default = "default_mapi_lite_total_timeout_seconds")]
    pub total_timeout_seconds: u64,
}

/// One attempt's ceiling.
///
/// Chosen with `max_retries` and `total_timeout_seconds` so all three attempts
/// actually fit: 3 x 13s of attempts plus 3s of back-off is 42s, inside the
/// 45s deadline. It was 30s, which made the deadline cut the sequence off
/// after one attempt and part of a second, so the stated retry count was not
/// what ran. The worst case a `/fund` caller waits is unchanged at 45s; what
/// changed is that the retries inside it are real.
fn default_mapi_lite_timeout_seconds() -> u64 {
    13
}

fn default_mapi_lite_health_timeout_seconds() -> u64 {
    2
}

fn default_mapi_lite_max_retries() -> u32 {
    2
}

fn default_mapi_lite_total_timeout_seconds() -> u64 {
    45
}

/// Back-off between submit attempts, mirrored from `broadcaster::mapi` so the
/// budget arithmetic below describes what the broadcaster actually does.
const RETRY_BACKOFF_BASE_SECONDS: u64 = 1;
const RETRY_BACKOFF_CAP_SECONDS: u64 = 5;

/// `/ready` answers orchestrator readiness probes, which allow a probe a few
/// seconds at most (Docker's health check defaults to `--timeout=3s`). A
/// mapi-lite probe slower than that is killed by the caller rather than
/// answered, marking the instance unready on every check even while mapi-lite
/// is fine -- so the bound is rejected at startup rather than only documented.
const MAPI_LITE_HEALTH_TIMEOUT_LIMIT_SECONDS: u64 = 3;

impl MapiLiteConfig {
    /// A section naming only `base_url`, with every other field at its default.
    #[cfg(test)]
    pub fn for_base_url(base_url: impl Into<String>) -> Self {
        MapiLiteConfig {
            base_url: base_url.into(),
            auth_token: None,
            timeout_seconds: default_mapi_lite_timeout_seconds(),
            health_timeout_seconds: default_mapi_lite_health_timeout_seconds(),
            max_retries: default_mapi_lite_max_retries(),
            total_timeout_seconds: default_mapi_lite_total_timeout_seconds(),
        }
    }

    /// The configured base URL, normalised: surrounding whitespace removed and
    /// any trailing `/` stripped, so `uls-client` always joins its paths onto
    /// a bare origin. Validation, the HTTP clients and the log lines all read
    /// the URL through here, so a padded or slash-terminated value in the TOML
    /// cannot validate as one thing and be requested as another.
    pub fn base_url(&self) -> &str {
        self.base_url.trim().trim_end_matches('/')
    }

    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_seconds)
    }

    pub fn health_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.health_timeout_seconds)
    }

    pub fn total_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.total_timeout_seconds)
    }

    /// How many submit attempts can actually start before the deadline cuts
    /// the sequence off.
    ///
    /// The three fields interact and nothing makes that obvious: an operator
    /// who raises `max_retries` for resilience may get no extra attempts at
    /// all. This walks the sequence the broadcaster runs -- attempt, back-off,
    /// attempt -- against `total_timeout_seconds` and reports what will
    /// really happen, assuming the worst case where every attempt runs to its
    /// full timeout.
    pub fn attempts_within_deadline(&self) -> u32 {
        let mut elapsed = 0u64;
        let mut started = 0u32;
        for attempt in 1..=(self.max_retries + 1) {
            if elapsed >= self.total_timeout_seconds {
                break;
            }
            started = attempt;
            elapsed += self
                .timeout_seconds
                .min(self.total_timeout_seconds - elapsed);
            let backoff =
                (RETRY_BACKOFF_BASE_SECONDS * attempt as u64).min(RETRY_BACKOFF_CAP_SECONDS);
            elapsed += backoff;
        }
        started
    }

    /// A warning for a section whose retry budget cannot fit its deadline, or
    /// `None` when the three fields agree.
    ///
    /// Warn rather than reject: the combination still works, it just does
    /// fewer attempts than it says, and refusing to start would break
    /// deployments that have been running on it.
    pub fn retry_budget_warning(&self) -> Option<String> {
        let configured = self.max_retries + 1;
        let actual = self.attempts_within_deadline();
        if actual >= configured {
            return None;
        }
        Some(format!(
            "mapi_lite: max_retries = {} asks for {configured} submit attempts, but only {actual} \
             can start within total_timeout_seconds = {}s at timeout_seconds = {}s (plus \
             back-off). Raise total_timeout_seconds or lower timeout_seconds/max_retries so the \
             three agree.",
            self.max_retries, self.total_timeout_seconds, self.timeout_seconds
        ))
    }

    /// Reject a section the broadcaster cannot work with, at startup rather
    /// than on the first funding request.
    pub fn validate(&self) -> Result<(), String> {
        let base_url = self.base_url();
        if base_url.is_empty() {
            return Err("mapi_lite.base_url is required when [mapi_lite] is present".to_string());
        }
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(format!(
                "mapi_lite.base_url must start with http:// or https://, got '{base_url}'"
            ));
        }
        if self.timeout_seconds == 0 {
            return Err("mapi_lite.timeout_seconds must be greater than zero".to_string());
        }
        if self.health_timeout_seconds == 0 {
            return Err("mapi_lite.health_timeout_seconds must be greater than zero".to_string());
        }
        if self.health_timeout_seconds >= MAPI_LITE_HEALTH_TIMEOUT_LIMIT_SECONDS {
            return Err(format!(
                "mapi_lite.health_timeout_seconds must be less than \
                 {MAPI_LITE_HEALTH_TIMEOUT_LIMIT_SECONDS}, so GET /ready answers inside a \
                 readiness probe's own timeout, got {}",
                self.health_timeout_seconds
            ));
        }
        if self.total_timeout_seconds == 0 {
            return Err("mapi_lite.total_timeout_seconds must be greater than zero".to_string());
        }
        if self.total_timeout_seconds < self.timeout_seconds {
            return Err(format!(
                "mapi_lite.total_timeout_seconds ({}) must be at least timeout_seconds ({}), \
                 otherwise not even the first attempt can finish",
                self.total_timeout_seconds, self.timeout_seconds
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct DynamicConfigConfig {
    pub filename: String,
}

/// Per-IP HTTP rate limiting configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_requests_per_second")]
    pub requests_per_second: u64,
    #[serde(default)]
    pub burst_size: Option<u32>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            enabled: false,
            requests_per_second: default_requests_per_second(),
            burst_size: None,
        }
    }
}

fn default_requests_per_second() -> u64 {
    10
}

impl RateLimitConfig {
    pub fn effective_burst_size(&self) -> u32 {
        self.burst_size
            .unwrap_or(self.requests_per_second.min(u32::MAX as u64) as u32)
            .max(1)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.requests_per_second == 0 {
            return Err(
                "web_interface.rate_limit.requests_per_second must be greater than 0 when rate limiting is enabled".to_string(),
            );
        }
        Ok(())
    }
}

/// Web Interface Configuration
#[derive(Debug, Deserialize, Clone)]
pub struct WebInterfaceConfig {
    pub address: Ipv4Addr,
    pub port: u16,
    /// When set, required for `POST /client`.
    #[serde(default)]
    pub admin_api_key: Option<String>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

impl Default for WebInterfaceConfig {
    fn default() -> Self {
        WebInterfaceConfig {
            address: Ipv4Addr::new(0, 0, 0, 0),
            port: 0,
            admin_api_key: None,
            rate_limit: RateLimitConfig::default(),
        }
    }
}

/// Service Configuration
#[derive(Debug, Default, Deserialize, Clone)]
pub struct Config {
    pub blockchain_interface: BlockchainInterfaceConfig,
    pub web_interface: WebInterfaceConfig,
    pub logging: LoggingConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    pub service: ServiceConfig,
    pub client: Option<Vec<ClientConfig>>,
    pub dynamic_config: DynamicConfigConfig,
    #[serde(default)]
    pub idempotency: IdempotencyConfig,
    /// Present => funding transactions are broadcast via mapi-lite.
    #[serde(default)]
    pub mapi_lite: Option<MapiLiteConfig>,
    #[serde(default)]
    pub fees: FeesConfig,
}

impl ClientConfig {
    /// Resolve `env:VAR` references and client-specific environment overrides.
    pub fn resolve_secrets(self) -> Result<Self, String> {
        resolve_client_config(self)
    }
}

impl Config {
    /// Return the configured network as Network type
    pub fn get_network(&self) -> Result<Network, &str> {
        match self.blockchain_interface.network_type.as_str() {
            "mainnet" => Ok(Network::BSV_Mainnet),
            "testnet" => Ok(Network::BSV_Testnet),
            "stn" => Ok(Network::BSV_STN),
            "regtest" => Ok(Network::BSV_Regtest),
            _ => Err("unable to decode network"),
        }
    }

    // Return the log level (as a log::Level type) from the config
    pub fn get_log_level(&self) -> Result<log::Level, String> {
        match self.logging.level.as_str() {
            "error" => Ok(log::Level::Error),
            "warn" | "warning" => Ok(log::Level::Warn),
            "info" | "information" => Ok(log::Level::Info),
            "debug" => Ok(log::Level::Debug),
            "trace" => Ok(log::Level::Trace),
            other => Err(format!("Unknown log level '{other}'")),
        }
    }

    /// Resolve secret references and apply environment overrides.
    pub fn resolve_secrets(mut self) -> Result<Self, String> {
        if let Ok(admin_api_key) = env::var("FS_ADMIN_API_KEY") {
            if !admin_api_key.is_empty() {
                self.web_interface.admin_api_key = Some(admin_api_key);
            }
        } else if let Some(admin_api_key) = self.web_interface.admin_api_key.take() {
            self.web_interface.admin_api_key = Some(resolve_secret(&admin_api_key)?);
        }

        // The node's RPC credentials are secrets like any other.
        if let Ok(rpc_user) = env::var("FS_RPC_USER") {
            if !rpc_user.is_empty() {
                self.blockchain_interface.rpc_user = Some(rpc_user);
            }
        } else if let Some(rpc_user) = self.blockchain_interface.rpc_user.take() {
            self.blockchain_interface.rpc_user = Some(resolve_secret(&rpc_user)?);
        }

        if let Ok(rpc_password) = env::var("FS_RPC_PASSWORD") {
            if !rpc_password.is_empty() {
                self.blockchain_interface.rpc_password = Some(rpc_password);
            }
        } else if let Some(rpc_password) = self.blockchain_interface.rpc_password.take() {
            self.blockchain_interface.rpc_password = Some(resolve_secret(&rpc_password)?);
        }

        // The mapi-lite token is a bearer credential like the API keys.
        if let Some(mapi_lite) = self.mapi_lite.as_mut() {
            if let Ok(auth_token) = env::var("FS_MAPI_LITE_AUTH_TOKEN") {
                if !auth_token.is_empty() {
                    mapi_lite.auth_token = Some(auth_token);
                } else if let Some(auth_token) = mapi_lite.auth_token.take() {
                    mapi_lite.auth_token = Some(resolve_secret(&auth_token)?);
                }
            } else if let Some(auth_token) = mapi_lite.auth_token.take() {
                mapi_lite.auth_token = Some(resolve_secret(&auth_token)?);
            }
            // An empty token and no token mean the same thing: send none.
            if mapi_lite.auth_token.as_deref() == Some("") {
                mapi_lite.auth_token = None;
            }
        }

        if let Some(clients) = self.client.as_mut() {
            let resolved = std::mem::take(clients)
                .into_iter()
                .map(resolve_client_config)
                .collect::<Result<Vec<_>, _>>()?;
            *clients = resolved;
        }

        Ok(self)
    }
}

fn resolve_client_config(mut client: ClientConfig) -> Result<ClientConfig, String> {
    if let Ok(wif_key) = env::var(client_wif_env_var(&client.client_id)) {
        if !wif_key.is_empty() {
            client.wif_key = wif_key;
        } else {
            client.wif_key = resolve_secret(&client.wif_key)?;
        }
    } else {
        client.wif_key = resolve_secret(&client.wif_key)?;
    }

    if let Ok(api_key) = env::var(client_api_key_env_var(&client.client_id)) {
        if !api_key.is_empty() {
            client.api_key = Some(api_key);
        } else if let Some(key) = client.api_key.take() {
            client.api_key = Some(resolve_secret(&key)?);
        }
    } else if let Some(api_key) = client.api_key.take() {
        client.api_key = Some(resolve_secret(&api_key)?);
    }

    Ok(client)
}

fn client_wif_env_var(client_id: &str) -> String {
    format!("FS_CLIENT_{}_WIF", env_key_suffix(client_id))
}

fn client_api_key_env_var(client_id: &str) -> String {
    format!("FS_CLIENT_{}_API_KEY", env_key_suffix(client_id))
}

fn env_key_suffix(client_id: &str) -> String {
    client_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Return the HTTP bind address and port, honouring `APP_ENV=docker`.
pub fn web_bind_address(config: &Config) -> (Ipv4Addr, u16) {
    let port = config.web_interface.port;
    match env::var_os("APP_ENV") {
        Some(content) if content == "docker" => (Ipv4Addr::new(0, 0, 0, 0), port),
        Some(_) | None => (config.web_interface.address, port),
    }
}

pub fn load_config(env_var: &str, filename: &str) -> Result<Config, String> {
    let config = get_config(env_var, filename)?;
    config.web_interface.rate_limit.validate()?;
    config.telemetry.validate()?;
    config.idempotency.validate()?;
    config.blockchain_interface.validate()?;
    config.fees.validate()?;
    if let Some(mapi_lite) = &config.mapi_lite {
        mapi_lite.validate()?;
    }
    warn_plaintext_secrets(&config);
    config.resolve_secrets()
}

/// Read the config from the provided file
fn read_config(filename: &str) -> Result<Config, String> {
    debug!("read_config = {}", filename);
    // Given filename read the config
    let content = std::fs::read_to_string(filename).map_err(|e| e.to_string())?;
    let config = toml::from_str(&content).map_err(|e| e.to_string())?;
    Ok(config)
}

/// Read the config from environment variable, if not read from filename
pub fn get_config(env_var: &str, filename: &str) -> Result<Config, String> {
    match env::var_os(env_var) {
        Some(content) => {
            let val = content
                .into_string()
                .map_err(|_| format!("Environment variable {env_var} is not valid UTF-8"))?;
            serde_json::from_str(&val)
                .map_err(|e| format!("Error parsing JSON environment variable {env_var}: {e}"))
        }
        None => {
            read_config(filename).map_err(|e| format!("Error reading config file {filename}: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chain_gang::network::Network;

    use crate::test_support::env_lock;

    #[test]
    fn resolve_secrets_replaces_env_references() {
        let _env = env_lock();
        unsafe { env::set_var("FS_TEST_CONFIG_WIF", "test-wif-value") };
        unsafe { env::set_var("FS_TEST_CONFIG_API_KEY", "test-api-key") };
        unsafe { env::set_var("FS_TEST_CONFIG_ADMIN", "admin-from-env") };
        unsafe { env::remove_var("FS_CLIENT_ID1_WIF") };

        let config = Config {
            web_interface: WebInterfaceConfig {
                address: Ipv4Addr::new(127, 0, 0, 1),
                port: 8080,
                admin_api_key: Some("env:FS_TEST_CONFIG_ADMIN".to_string()),
                rate_limit: RateLimitConfig::default(),
            },
            client: Some(vec![ClientConfig {
                client_id: "id1".to_string(),
                wif_key: "env:FS_TEST_CONFIG_WIF".to_string(),
                api_key: Some("env:FS_TEST_CONFIG_API_KEY".to_string()),
            }]),
            ..Default::default()
        };

        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(
            resolved.web_interface.admin_api_key.as_deref(),
            Some("admin-from-env")
        );
        let client = resolved.client.as_ref().unwrap().first().unwrap();
        assert_eq!(client.wif_key, "test-wif-value");
        assert_eq!(client.api_key.as_deref(), Some("test-api-key"));
    }

    #[test]
    fn resolve_secrets_applies_client_env_overrides() {
        let _env = env_lock();
        unsafe { env::set_var("FS_CLIENT_ID1_WIF", "override-wif") };
        let config = Config {
            client: Some(vec![ClientConfig {
                client_id: "id1".to_string(),
                wif_key: "env:SHOULD_NOT_BE_USED".to_string(),
                api_key: None,
            }]),
            ..Default::default()
        };
        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(resolved.client.as_ref().unwrap()[0].wif_key, "override-wif");
        unsafe { env::remove_var("FS_CLIENT_ID1_WIF") };
    }

    #[test]
    fn sr_sec_008_resolve_secrets_applies_client_api_key_env_override() {
        let _env = env_lock();
        unsafe { env::set_var("FS_CLIENT_ID1_API_KEY", "override-api-key") };
        let config = Config {
            client: Some(vec![ClientConfig {
                client_id: "id1".to_string(),
                wif_key: "env:FS_TEST_CONFIG_WIF".to_string(),
                api_key: Some("env:SHOULD_NOT_BE_USED".to_string()),
            }]),
            ..Default::default()
        };
        unsafe { env::set_var("FS_TEST_CONFIG_WIF", "test-wif-value") };
        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(
            resolved.client.as_ref().unwrap()[0].api_key.as_deref(),
            Some("override-api-key")
        );
        unsafe {
            env::remove_var("FS_CLIENT_ID1_API_KEY");
            env::remove_var("FS_TEST_CONFIG_WIF");
        }
    }

    #[test]
    fn rate_limit_config_rejects_zero_requests_per_second_when_enabled() {
        let config = RateLimitConfig {
            enabled: true,
            requests_per_second: 0,
            burst_size: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn sr_cfg_001_load_config_reads_toml_file() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "financing-service-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[blockchain_interface]
interface_type = "test"
network_type = "testnet"

[web_interface]
address = "127.0.0.1"
port = 9091

[logging]
level = "info"

[service]
utxo_refresh_period = 45

[dynamic_config]
filename = "./data/dynamic.toml"
"#,
        )
        .unwrap();

        unsafe { env::remove_var("FS_CONFIG") };
        let config = load_config("FS_CONFIG", path.to_str().unwrap()).unwrap();
        assert_eq!(config.web_interface.port, 9091);
        assert_eq!(config.service.utxo_refresh_period, 45);
    }

    #[test]
    fn sr_cfg_002_get_config_reads_fs_config_json() {
        let _env = env_lock();
        unsafe {
            env::set_var(
                "FS_CONFIG",
                r#"{"blockchain_interface":{"interface_type":"test","network_type":"testnet"},"web_interface":{"address":"127.0.0.1","port":9092},"logging":{"level":"info"},"service":{"utxo_refresh_period":30},"dynamic_config":{"filename":"./data/dynamic.toml"}}"#,
            );
        }
        let config = get_config("FS_CONFIG", "missing-file.toml").unwrap();
        assert_eq!(config.web_interface.port, 9092);
        unsafe { env::remove_var("FS_CONFIG") };
    }

    #[test]
    fn sr_cfg_003_web_bind_address_uses_all_interfaces_in_docker() {
        let _env = env_lock();
        unsafe { env::set_var("APP_ENV", "docker") };
        let config = Config {
            web_interface: WebInterfaceConfig {
                address: Ipv4Addr::new(127, 0, 0, 1),
                port: 8080,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(web_bind_address(&config), (Ipv4Addr::new(0, 0, 0, 0), 8080));
        unsafe { env::remove_var("APP_ENV") };
    }

    #[test]
    fn sr_cfg_006_get_log_level_accepts_configured_levels() {
        for (level, expected) in [
            ("error", log::Level::Error),
            ("warn", log::Level::Warn),
            ("info", log::Level::Info),
            ("debug", log::Level::Debug),
            ("trace", log::Level::Trace),
        ] {
            let config = Config {
                logging: LoggingConfig {
                    level: level.to_string(),
                },
                ..Default::default()
            };
            assert_eq!(config.get_log_level().unwrap(), expected);
        }
    }

    #[test]
    fn sr_bchn_002_get_network_supports_mainnet_testnet_stn_and_regtest() {
        for (network_type, expected) in [
            ("mainnet", Network::BSV_Mainnet),
            ("testnet", Network::BSV_Testnet),
            ("stn", Network::BSV_STN),
            ("regtest", Network::BSV_Regtest),
        ] {
            let config = Config {
                blockchain_interface: BlockchainInterfaceConfig {
                    network_type: network_type.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            };
            assert_eq!(config.get_network().unwrap(), expected);
        }
    }

    fn rpc_interface_config(
        url: Option<&str>,
        user: Option<&str>,
        password: Option<&str>,
    ) -> BlockchainInterfaceConfig {
        BlockchainInterfaceConfig {
            interface_type: "rpc".to_string(),
            network_type: "regtest".to_string(),
            url: url.map(str::to_string),
            rpc_user: user.map(str::to_string),
            rpc_password: password.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn sr_bchn_005_rpc_config_requires_address_and_credentials() {
        assert!(
            rpc_interface_config(Some("127.0.0.1:18443"), Some("u"), Some("p"))
                .validate()
                .is_ok()
        );
        for (url, user, password, expected) in [
            (None, Some("u"), Some("p"), "url"),
            (Some(""), Some("u"), Some("p"), "url"),
            (Some("127.0.0.1:18443"), None, Some("p"), "rpc_user"),
            (Some("127.0.0.1:18443"), Some("u"), None, "rpc_password"),
        ] {
            let error = rpc_interface_config(url, user, password)
                .validate()
                .expect_err("expected validation to fail");
            assert!(error.contains(expected), "should name {expected}: {error}");
        }
    }

    /// Imports are on unless turned off, because the failure they prevent is
    /// silent.
    #[test]
    fn rpc_import_addresses_defaults_to_on() {
        let config: BlockchainInterfaceConfig = toml::from_str(
            "interface_type = \"rpc\"\nnetwork_type = \"regtest\"\nurl = \"127.0.0.1:18443\"",
        )
        .unwrap();
        assert!(config.rpc_import_addresses);
    }

    #[test]
    fn woc_and_test_interfaces_need_no_url() {
        for interface_type in ["woc", "test"] {
            let config = BlockchainInterfaceConfig {
                interface_type: interface_type.to_string(),
                network_type: "testnet".to_string(),
                ..Default::default()
            };
            assert!(config.validate().is_ok());
        }
    }

    #[test]
    fn sr_sec_014_rpc_password_resolves_from_an_env_reference() {
        let _guard = crate::test_support::env_lock();
        unsafe { std::env::set_var("FS_TEST_RPC_PASSWORD", "from-env") };
        unsafe { std::env::remove_var("FS_RPC_PASSWORD") };
        unsafe { std::env::remove_var("FS_RPC_USER") };
        let config = Config {
            blockchain_interface: rpc_interface_config(
                Some("127.0.0.1:18443"),
                Some("rpcuser"),
                Some("env:FS_TEST_RPC_PASSWORD"),
            ),
            ..Default::default()
        };
        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(
            resolved.blockchain_interface.rpc_password.as_deref(),
            Some("from-env")
        );
        unsafe { std::env::remove_var("FS_TEST_RPC_PASSWORD") };
    }

    #[test]
    fn sr_sec_014_fs_rpc_password_env_overrides_the_config() {
        let _guard = crate::test_support::env_lock();
        unsafe { std::env::set_var("FS_RPC_PASSWORD", "override") };
        let config = Config {
            blockchain_interface: rpc_interface_config(
                Some("127.0.0.1:18443"),
                Some("rpcuser"),
                Some("in-file"),
            ),
            ..Default::default()
        };
        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(
            resolved.blockchain_interface.rpc_password.as_deref(),
            Some("override")
        );
        unsafe { std::env::remove_var("FS_RPC_PASSWORD") };
    }

    #[test]
    fn sr_sec_014_plaintext_rpc_password_is_reported() {
        let config = Config {
            blockchain_interface: rpc_interface_config(
                Some("127.0.0.1:18443"),
                Some("rpcuser"),
                Some("plaintext-secret"),
            ),
            ..Default::default()
        };
        assert!(crate::secrets::plaintext_secret_fields(&config)
            .contains(&"blockchain_interface.rpc_password".to_string()));
    }

    #[test]
    fn sr_clnt_001_config_supports_multiple_static_clients() {
        let config = Config {
            client: Some(vec![
                ClientConfig {
                    client_id: "id1".to_string(),
                    wif_key: "wif1".to_string(),
                    api_key: None,
                },
                ClientConfig {
                    client_id: "id2".to_string(),
                    wif_key: "wif2".to_string(),
                    api_key: None,
                },
            ]),
            ..Default::default()
        };
        assert_eq!(config.client.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn sr_nfr_007_load_config_returns_error_for_invalid_file() {
        let _env = env_lock();
        unsafe { env::remove_var("FS_CONFIG") };
        let err = load_config("FS_CONFIG", "definitely-missing-config.toml").unwrap_err();
        assert!(err.contains("Error reading config file"));
    }

    #[test]
    fn sr_bchn_003_sample_config_sets_utxo_refresh_period() {
        let content = std::fs::read_to_string("data/financing-service.toml").unwrap();
        let config: Config = toml::from_str(&content).unwrap();
        assert_eq!(config.service.utxo_refresh_period, 60);
    }

    #[test]
    fn sr_cfg_007_load_config_validates_telemetry_config() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "financing-service-telemetry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[blockchain_interface]
interface_type = "test"
network_type = "testnet"

[web_interface]
address = "127.0.0.1"
port = 8080

[logging]
level = "info"

[telemetry]
enabled = true
otlp_endpoint = ""

[service]
utxo_refresh_period = 60

[dynamic_config]
filename = "./data/dynamic.toml"
"#,
        )
        .unwrap();

        unsafe { env::remove_var("FS_CONFIG") };
        let err = load_config("FS_CONFIG", path.to_str().unwrap()).unwrap_err();
        assert!(err.contains("telemetry.otlp_endpoint"));
    }

    /// The smallest config the service accepts, for the [mapi_lite] tests to
    /// build on.
    const MINIMAL_TOML: &str = r#"
[blockchain_interface]
interface_type = "test"
network_type = "testnet"

[web_interface]
address = "127.0.0.1"
port = 8080

[logging]
level = "info"

[service]
utxo_refresh_period = 60

[dynamic_config]
filename = "./data/dynamic.toml"
"#;

    /// A config with no [fees] section is valid and takes the documented
    /// defaults, so CS-451 does not make the section mandatory.
    #[test]
    fn cs_451_the_fees_section_is_optional() {
        let config: Config = toml::from_str(MINIMAL_TOML).unwrap();
        assert_eq!(config.fees.satoshis_per_kb, DEFAULT_SATOSHIS_PER_KB);
        assert_eq!(config.fees.satoshis_per_kb, 100);
        assert!(
            config.fees.use_mapi_fee_quote,
            "the quote is used by default when there is a mapi-lite to ask"
        );
        assert_eq!(
            config.fees.dust_threshold_satoshis,
            DEFAULT_DUST_THRESHOLD_SATOSHIS
        );
        assert_eq!(config.fees.dust_threshold_satoshis, 135);
        assert!(config.fees.validate().is_ok());
    }

    /// Either field can be set on its own; the other keeps its default.
    #[test]
    fn cs_451_each_fee_field_defaults_independently() {
        let only_rate: Config =
            toml::from_str(&format!("{MINIMAL_TOML}\n[fees]\nsatoshis_per_kb = 250\n")).unwrap();
        assert_eq!(only_rate.fees.satoshis_per_kb, 250);
        assert!(only_rate.fees.use_mapi_fee_quote);

        let only_flag: Config = toml::from_str(&format!(
            "{MINIMAL_TOML}\n[fees]\nuse_mapi_fee_quote = false\n"
        ))
        .unwrap();
        assert_eq!(only_flag.fees.satoshis_per_kb, DEFAULT_SATOSHIS_PER_KB);
        assert!(!only_flag.fees.use_mapi_fee_quote);

        let only_dust: Config = toml::from_str(&format!(
            "{MINIMAL_TOML}\n[fees]\ndust_threshold_satoshis = 0\n"
        ))
        .unwrap();
        assert_eq!(only_dust.fees.dust_threshold_satoshis, 0);
        assert_eq!(only_dust.fees.satoshis_per_kb, DEFAULT_SATOSHIS_PER_KB);
        assert!(
            only_dust.fees.validate().is_ok(),
            "zero is a valid threshold: it means pay every change back"
        );
    }

    /// A zero rate builds transactions nothing relays, and the service would
    /// only find that out at broadcast. It fails at startup instead, naming
    /// the field.
    #[test]
    fn cs_451_a_zero_rate_is_refused_at_startup() {
        let _env = env_lock();
        let path = write_temp_config(
            "fees-zero",
            &format!("{MINIMAL_TOML}\n[fees]\nsatoshis_per_kb = 0\n"),
        );
        unsafe { env::remove_var("FS_CONFIG") };
        let err = load_config("FS_CONFIG", path.to_str().unwrap()).unwrap_err();
        assert!(err.contains("fees.satoshis_per_kb"), "{err}");
    }

    fn write_temp_config(label: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "financing-service-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    /// The minimal config plus a `[mapi_lite]` section holding `fields`.
    fn with_mapi_lite_section(fields: &str) -> String {
        format!("{MINIMAL_TOML}\n[mapi_lite]\n{fields}")
    }

    fn config_with_mapi_lite(auth_token: Option<&str>) -> Config {
        let mut mapi_lite = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        mapi_lite.auth_token = auth_token.map(str::to_string);
        Config {
            mapi_lite: Some(mapi_lite),
            ..Default::default()
        }
    }

    fn plaintext_fields(auth_token: Option<&str>) -> Vec<String> {
        crate::secrets::plaintext_secret_fields(&config_with_mapi_lite(auth_token))
    }

    /// Leaving the section out is the WoC deployment, and must keep working
    /// with no change to existing config files.
    #[test]
    fn sr_cfg_008_config_without_a_mapi_lite_section_has_none() {
        let config: Config = toml::from_str(MINIMAL_TOML).unwrap();
        assert!(config.mapi_lite.is_none());
    }

    #[test]
    fn sr_cfg_008_mapi_lite_section_parses_with_defaults() {
        let content = with_mapi_lite_section("base_url = \"http://127.0.0.1:8080\"\n");
        let config: Config = toml::from_str(&content).unwrap();
        let mapi_lite = config.mapi_lite.expect("section present");
        assert_eq!(mapi_lite.base_url, "http://127.0.0.1:8080");
        assert_eq!(mapi_lite.auth_token, None);
        assert_eq!(mapi_lite.timeout_seconds, 13);
        assert_eq!(mapi_lite.health_timeout_seconds, 2);
        assert_eq!(mapi_lite.max_retries, 2);
        assert_eq!(mapi_lite.total_timeout_seconds, 45);
        assert!(mapi_lite.validate().is_ok());
    }

    /// The defaults have to be a set that can all happen at once, or
    /// `max_retries` is describing attempts that never run.
    #[test]
    fn sr_cfg_009_the_default_retry_budget_fits_the_default_deadline() {
        let config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        assert_eq!(
            config.attempts_within_deadline(),
            config.max_retries + 1,
            "all {} attempts should fit in {}s",
            config.max_retries + 1,
            config.total_timeout_seconds
        );
        assert_eq!(config.retry_budget_warning(), None);
    }

    /// The combination the issue describes: three attempts asked for, one and
    /// a bit delivered. It is legal, so it still starts -- but it says so.
    #[test]
    fn sr_cfg_009_a_budget_that_cannot_fit_is_warned_about() {
        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        config.timeout_seconds = 30;
        config.max_retries = 2;
        config.total_timeout_seconds = 45;

        assert_eq!(config.attempts_within_deadline(), 2);
        let warning = config
            .retry_budget_warning()
            .expect("an unfittable budget is warned about");
        assert!(warning.contains("3 submit attempts"), "{warning}");
        assert!(warning.contains("only 2"), "{warning}");
        // still a legal section: warn, do not refuse to start
        assert!(config.validate().is_ok());
    }

    /// Raising `max_retries` alone is the trap the issue names: without more
    /// deadline it buys nothing, and the operator is told so.
    #[test]
    fn sr_cfg_009_raising_max_retries_alone_is_warned_about() {
        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        config.max_retries = 10;
        let attempts = config.attempts_within_deadline();
        assert!(
            attempts < 11,
            "11 attempts cannot fit 45s at 13s each, got {attempts}"
        );
        assert!(config.retry_budget_warning().is_some());
    }

    /// Whatever `validate` checked has to be what the client requests, so the
    /// normalised URL -- not the raw field -- is what both of them read.
    #[test]
    fn sr_cfg_008_mapi_lite_base_url_is_normalised_before_use() {
        for raw in [
            "http://mapi:8080",
            "http://mapi:8080/",
            "http://mapi:8080///",
            "  http://mapi:8080  ",
            "\thttp://mapi:8080/\n",
        ] {
            let config = MapiLiteConfig::for_base_url(raw);
            assert_eq!(config.base_url(), "http://mapi:8080", "for {raw:?}");
            assert!(config.validate().is_ok(), "for {raw:?}");
        }

        // A padded value that is not a URL is still rejected: validation reads
        // the same normalised string the client would be built from.
        let error = MapiLiteConfig::for_base_url(" ftp://mapi ")
            .validate()
            .expect_err("not an http URL");
        assert!(
            error.contains("must start with http:// or https://"),
            "{error}"
        );
    }

    #[test]
    fn sr_cfg_008_mapi_lite_section_accepts_every_field() {
        let content = with_mapi_lite_section(
            "base_url = \"https://mapi.example:8443/\"\nauth_token = \"Bearer secret\"\ntimeout_seconds = 5\nhealth_timeout_seconds = 1\nmax_retries = 0\n",
        );
        let config: Config = toml::from_str(&content).unwrap();
        let mapi_lite = config.mapi_lite.expect("section present");
        assert_eq!(mapi_lite.base_url, "https://mapi.example:8443/");
        assert_eq!(mapi_lite.auth_token.as_deref(), Some("Bearer secret"));
        assert_eq!(mapi_lite.timeout(), std::time::Duration::from_secs(5));
        assert_eq!(
            mapi_lite.health_timeout(),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(mapi_lite.max_retries, 0);
        assert!(mapi_lite.validate().is_ok());
    }

    #[test]
    fn sr_cfg_008_mapi_lite_validate_rejects_a_missing_or_non_http_base_url() {
        for (base_url, expected) in [
            ("", "mapi_lite.base_url is required"),
            ("   ", "mapi_lite.base_url is required"),
            ("127.0.0.1:8080", "must start with http:// or https://"),
            ("ftp://mapi", "must start with http:// or https://"),
        ] {
            let error = MapiLiteConfig::for_base_url(base_url)
                .validate()
                .expect_err("expected validation to fail");
            assert!(error.contains(expected), "for '{base_url}': {error}");
        }
    }

    #[test]
    fn sr_cfg_008_mapi_lite_validate_rejects_zero_timeouts() {
        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        config.timeout_seconds = 0;
        assert!(config
            .validate()
            .unwrap_err()
            .contains("mapi_lite.timeout_seconds"));

        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        config.health_timeout_seconds = 0;
        assert!(config
            .validate()
            .unwrap_err()
            .contains("mapi_lite.health_timeout_seconds"));

        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        config.total_timeout_seconds = 0;
        assert!(config
            .validate()
            .unwrap_err()
            .contains("mapi_lite.total_timeout_seconds"));
    }

    /// A mapi-lite probe slower than the few seconds a readiness probe allows
    /// is killed by the caller rather than answered, marking the instance
    /// unready on every check even while mapi-lite is fine. So the documented
    /// ceiling is enforced rather than just written down.
    #[test]
    fn sr_cfg_008_mapi_lite_validate_rejects_a_health_timeout_a_probe_cannot_wait_for() {
        for seconds in [3, 4, 10] {
            let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
            config.health_timeout_seconds = seconds;
            let error = config
                .validate()
                .expect_err("a health timeout at or above 3s is rejected");
            assert!(
                error.contains("must be less than 3"),
                "for {seconds}s: {error}"
            );
        }

        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        config.health_timeout_seconds = 2;
        assert!(config.validate().is_ok());
    }

    /// The retry budget multiplies `timeout_seconds`, so the total deadline
    /// has to leave room for at least one attempt.
    #[test]
    fn sr_cfg_008_mapi_lite_validate_rejects_a_total_timeout_below_one_attempt() {
        let mut config = MapiLiteConfig::for_base_url("http://127.0.0.1:8080");
        config.timeout_seconds = 30;
        config.total_timeout_seconds = 10;
        let error = config.validate().expect_err("no attempt can finish");
        assert!(
            error.contains("must be at least timeout_seconds"),
            "{error}"
        );

        config.total_timeout_seconds = 30;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn sr_cfg_008_load_config_validates_the_mapi_lite_section() {
        let _env = env_lock();
        let path = write_temp_config("mapi-lite", &with_mapi_lite_section("base_url = \"\"\n"));
        unsafe { env::remove_var("FS_CONFIG") };
        let err = load_config("FS_CONFIG", path.to_str().unwrap()).unwrap_err();
        assert!(err.contains("mapi_lite.base_url"), "{err}");
    }

    #[test]
    fn sr_cfg_008_load_config_reads_a_valid_mapi_lite_section() {
        let _env = env_lock();
        let path = write_temp_config(
            "mapi-lite-ok",
            &with_mapi_lite_section("base_url = \"http://127.0.0.1:8080\"\n"),
        );
        unsafe { env::remove_var("FS_CONFIG") };
        unsafe { env::remove_var("FS_MAPI_LITE_AUTH_TOKEN") };
        let config = load_config("FS_CONFIG", path.to_str().unwrap()).unwrap();
        assert_eq!(config.mapi_lite.unwrap().base_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn sr_cfg_008_fs_config_json_accepts_a_mapi_lite_object() {
        let _env = env_lock();
        unsafe {
            env::set_var(
                "FS_CONFIG",
                r#"{"blockchain_interface":{"interface_type":"test","network_type":"testnet"},"web_interface":{"address":"127.0.0.1","port":9092},"logging":{"level":"info"},"service":{"utxo_refresh_period":30},"dynamic_config":{"filename":"./data/dynamic.toml"},"mapi_lite":{"base_url":"http://mapi:8080","max_retries":1}}"#,
            );
        }
        let config = get_config("FS_CONFIG", "missing-file.toml").unwrap();
        unsafe { env::remove_var("FS_CONFIG") };
        let mapi_lite = config.mapi_lite.expect("object present");
        assert_eq!(mapi_lite.base_url, "http://mapi:8080");
        assert_eq!(mapi_lite.max_retries, 1);
    }

    #[test]
    fn sr_sec_015_mapi_lite_auth_token_resolves_from_an_env_reference() {
        let _env = env_lock();
        unsafe { env::set_var("FS_TEST_MAPI_TOKEN", "Bearer from-env") };
        unsafe { env::remove_var("FS_MAPI_LITE_AUTH_TOKEN") };
        let config = config_with_mapi_lite(Some("env:FS_TEST_MAPI_TOKEN"));
        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(
            resolved.mapi_lite.unwrap().auth_token.as_deref(),
            Some("Bearer from-env")
        );
        unsafe { env::remove_var("FS_TEST_MAPI_TOKEN") };
    }

    #[test]
    fn sr_sec_015_fs_mapi_lite_auth_token_env_overrides_the_config() {
        let _env = env_lock();
        unsafe { env::set_var("FS_MAPI_LITE_AUTH_TOKEN", "Bearer override") };
        let config = config_with_mapi_lite(Some("env:SHOULD_NOT_BE_USED"));
        let resolved = config.resolve_secrets().unwrap();
        assert_eq!(
            resolved.mapi_lite.unwrap().auth_token.as_deref(),
            Some("Bearer override")
        );
        unsafe { env::remove_var("FS_MAPI_LITE_AUTH_TOKEN") };
    }

    #[test]
    fn sr_sec_015_missing_mapi_lite_auth_token_env_reference_fails_resolution() {
        let _env = env_lock();
        unsafe { env::remove_var("FS_MAPI_LITE_AUTH_TOKEN") };
        unsafe { env::remove_var("FS_DEFINITELY_MISSING_MAPI_TOKEN") };
        let config = config_with_mapi_lite(Some("env:FS_DEFINITELY_MISSING_MAPI_TOKEN"));
        let err = config.resolve_secrets().unwrap_err();
        assert!(err.contains("FS_DEFINITELY_MISSING_MAPI_TOKEN"), "{err}");
    }

    #[test]
    fn mapi_lite_auth_token_absent_stays_absent_after_resolution() {
        let _env = env_lock();
        unsafe { env::remove_var("FS_MAPI_LITE_AUTH_TOKEN") };
        let resolved = config_with_mapi_lite(None).resolve_secrets().unwrap();
        assert_eq!(resolved.mapi_lite.unwrap().auth_token, None);
    }

    #[test]
    fn sr_sec_015_plaintext_mapi_lite_auth_token_is_reported() {
        let field = "mapi_lite.auth_token".to_string();
        assert!(plaintext_fields(Some("Bearer literal")).contains(&field));
        assert!(!plaintext_fields(Some("env:FS_MAPI_LITE_AUTH_TOKEN")).contains(&field));
        assert!(!plaintext_fields(None).contains(&field));
    }

    /// CS-418: the default has to honour what WhatsOnChain publishes, without
    /// the operator having to know the number. `woc` is somebody else's
    /// server; `rpc` and `uaas` are the operator's own and are left alone.
    #[test]
    fn cs_418_only_the_public_api_is_rate_limited_by_default() {
        let woc = BlockchainInterfaceConfig {
            interface_type: "woc".to_string(),
            ..Default::default()
        };
        assert_eq!(woc.outbound_rate_limit(), Some(WOC_REQUESTS_PER_SECOND));
        assert_eq!(
            WOC_REQUESTS_PER_SECOND, 3,
            "WhatsOnChain documents 'up to 3 requests/sec is free'"
        );

        for local in ["rpc", "uaas", "test"] {
            let config = BlockchainInterfaceConfig {
                interface_type: local.to_string(),
                ..Default::default()
            };
            assert_eq!(config.outbound_rate_limit(), None, "{local}");
        }
    }

    /// An operator with a paid plan, or a mirror of their own, can raise it;
    /// zero turns it off entirely. An explicit setting always wins over the
    /// per-interface default.
    #[test]
    fn cs_418_an_explicit_limit_overrides_the_default_either_way() {
        let faster = BlockchainInterfaceConfig {
            interface_type: "woc".to_string(),
            max_requests_per_second: Some(20),
            ..Default::default()
        };
        assert_eq!(faster.outbound_rate_limit(), Some(20));

        let off = BlockchainInterfaceConfig {
            interface_type: "woc".to_string(),
            max_requests_per_second: Some(0),
            ..Default::default()
        };
        assert_eq!(off.outbound_rate_limit(), None);

        let limited_node = BlockchainInterfaceConfig {
            interface_type: "rpc".to_string(),
            max_requests_per_second: Some(5),
            ..Default::default()
        };
        assert_eq!(limited_node.outbound_rate_limit(), Some(5));
    }
}
