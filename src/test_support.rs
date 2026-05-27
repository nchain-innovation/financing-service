use std::sync::Arc;

use chain_gang::interface::{BlockchainInterface, TestInterface, UtxoEntry};

use crate::config::{BlockchainInterfaceConfig, ClientConfig, Config, DynamicConfigConfig};

pub const TEST_WIF: &str = "cTYmKQzX3CvHJAxe2sctsQaHG8ktiEnpXgyyycVXGg5pRcLJEeLd";
pub const TEST_CLIENT_ID: &str = "id1";
pub const TEST_ADDRESS: &str = "n1jaAsKZfE6kufLy5DAtAyQ1RzGXwMeNAF";
pub const LOCKING_SCRIPT_HEX: &str = "76a914ddc574807c3035ab43553a22c0b9df1f55737fae88ac";

pub fn test_config(dynamic_filename: &str) -> Config {
    test_config_with_keys(dynamic_filename, None, None)
}

pub fn test_config_with_api_key(dynamic_filename: &str, api_key: Option<&str>) -> Config {
    test_config_with_keys(dynamic_filename, api_key, None)
}

pub fn test_config_with_keys(
    dynamic_filename: &str,
    client_api_key: Option<&str>,
    admin_api_key: Option<&str>,
) -> Config {
    Config {
        blockchain_interface: BlockchainInterfaceConfig {
            interface_type: "test".to_string(),
            network_type: "testnet".to_string(),
            url: None,
        },
        web_interface: crate::config::WebInterfaceConfig {
            address: std::net::Ipv4Addr::LOCALHOST,
            port: 8080,
            admin_api_key: admin_api_key.map(str::to_string),
        },
        client: Some(vec![ClientConfig {
            client_id: TEST_CLIENT_ID.to_string(),
            wif_key: TEST_WIF.to_string(),
            api_key: client_api_key.map(str::to_string),
        }]),
        dynamic_config: DynamicConfigConfig {
            filename: dynamic_filename.to_string(),
        },
        ..Default::default()
    }
}

pub fn test_utxo() -> Vec<UtxoEntry> {
    vec![
        UtxoEntry {
            height: 1514933,
            tx_pos: 0,
            tx_hash: "f67272e5c1408ecbeb8da543437c125ee1a17110317d44d13eafe31b771b795e".to_string(),
            value: 240,
        },
        UtxoEntry {
            height: 1514939,
            tx_pos: 1,
            tx_hash: "b3ec9a52a1fe1689a998c869c2ae38d64d08ece8aaf218286461f330f6fd2ca8".to_string(),
            value: 100,
        },
        UtxoEntry {
            height: 1514939,
            tx_pos: 1,
            tx_hash: "76f9302ab84fc5da40de02617c10f1a26dff7007c2bfffa9d0845e57d47fa82f".to_string(),
            value: 100,
        },
        UtxoEntry {
            height: 1514939,
            tx_pos: 1,
            tx_hash: "447ee285748e88d8b875ce09026815578f0474ec3f1babcd5ba917ecb9f1dd7a".to_string(),
            value: 100,
        },
        UtxoEntry {
            height: 1514939,
            tx_pos: 2,
            tx_hash: "447ee285748e88d8b875ce09026815578f0474ec3f1babcd5ba917ecb9f1dd7a".to_string(),
            value: 100,
        },
        UtxoEntry {
            height: 1516841,
            tx_pos: 0,
            tx_hash: "70d0365df8062e5af41f8e8f2e42bafde3cabbaebd1fc94e94fa5559e87777b2".to_string(),
            value: 39_080_962,
        },
        UtxoEntry {
            height: 1517272,
            tx_pos: 0,
            tx_hash: "51b349bda57674a02ea5b90b43f2204dd6df330d751d646d74d76b19348bf5be".to_string(),
            value: 39_327_675,
        },
        UtxoEntry {
            height: 1517429,
            tx_pos: 0,
            tx_hash: "e533109de9df0184299e2199fa8f74baae7d99e4b39d3dea1e957e2f26636578".to_string(),
            value: 9_564_208,
        },
        UtxoEntry {
            height: 1517429,
            tx_pos: 1,
            tx_hash: "e533109de9df0184299e2199fa8f74baae7d99e4b39d3dea1e957e2f26636578".to_string(),
            value: 123,
        },
    ]
}

pub async fn test_blockchain_interface(
    config: &Config,
) -> Arc<dyn BlockchainInterface + Send + Sync> {
    let mut blockchain_interface = TestInterface::new();
    blockchain_interface.set_network(&config.get_network().unwrap());
    blockchain_interface
        .set_utxo(TEST_ADDRESS, &test_utxo())
        .await;
    blockchain_interface.set_height(1517571).await;
    Arc::new(blockchain_interface)
}

pub fn unique_dynamic_config_path() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "financing-service-test-{}-{id}-dynamic.toml",
        std::process::id()
    ));
    let path = path.to_string_lossy().into_owned();
    let _ = std::fs::write(&path, "clients = []\n");
    path
}
