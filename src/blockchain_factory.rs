use std::sync::Arc;

use chain_gang::interface::{
    BlockchainInterface, RpcInterface, TestInterface, UaaSInterface, WocInterface,
};

use crate::address_watcher::AddressWatcher;
use crate::config::Config;

/// A configured backend, plus the means to tell it which addresses to watch
/// when it needs telling.
pub struct Backend {
    pub interface: Arc<dyn BlockchainInterface + Send + Sync>,
    /// `Some` only for backends that index nothing themselves. See
    /// [`crate::address_watcher`].
    pub address_watcher: Option<Arc<dyn AddressWatcher>>,
}

/// Takes a config and returns the appropriate configured object that implements BlockchainInterface
pub fn blockchain_factory(config: &Config) -> Result<Backend, String> {
    let network = config
        .get_network()
        .map_err(|_| "Unable to decode network from config".to_string())?;

    match config.blockchain_interface.interface_type.as_str() {
        "woc" => {
            let mut interface = WocInterface::new();
            interface.set_network(&network);
            Ok(Backend {
                interface: Arc::new(interface),
                address_watcher: None,
            })
        }
        "test" => {
            let mut interface = TestInterface::new();
            interface.set_network(&network);
            Ok(Backend {
                interface: Arc::new(interface),
                address_watcher: None,
            })
        }
        "uaas" => {
            let uaas_url = config
                .blockchain_interface
                .url
                .as_ref()
                .ok_or_else(|| "Config blockchain interface url not found.".to_string())?;
            let mut interface = UaaSInterface::new(uaas_url)
                .map_err(|e| format!("Unable to create UaaS interface: {e:?}"))?;
            interface.set_network(&network);
            Ok(Backend {
                interface: Arc::new(interface),
                address_watcher: None,
            })
        }
        "rpc" => {
            // A node we talk to directly. The only interface that reaches
            // regtest, since no public explorer indexes a private chain.
            let address = required(&config.blockchain_interface.url, "url")?;
            let user = required(&config.blockchain_interface.rpc_user, "rpc_user")?;
            let password = required(&config.blockchain_interface.rpc_password, "rpc_password")?;
            let interface = Arc::new(RpcInterface::new(address, user, password, network));
            // A node reports zero for an address its wallet does not track, so
            // it has to be told which ones to follow -- unless the operator
            // manages the wallet themselves.
            let address_watcher: Option<Arc<dyn AddressWatcher>> =
                if config.blockchain_interface.rpc_import_addresses {
                    Some(interface.clone())
                } else {
                    None
                };
            Ok(Backend {
                interface,
                address_watcher,
            })
        }
        other => Err(format!("Unknown interface type '{other}'")),
    }
}

/// Read a required `blockchain_interface` field, naming it if absent so the
/// operator is told which one to add rather than left guessing.
fn required<'a>(value: &'a Option<String>, field: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!("Config blockchain_interface.{field} is required for interface_type = \"rpc\".")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BlockchainInterfaceConfig;
    use chain_gang::network::Network;

    fn base_config(interface_type: &str, url: Option<&str>) -> Config {
        Config {
            blockchain_interface: BlockchainInterfaceConfig {
                interface_type: interface_type.to_string(),
                network_type: "testnet".to_string(),
                url: url.map(str::to_string),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn rpc_config(
        url: Option<&str>,
        user: Option<&str>,
        password: Option<&str>,
        import: bool,
    ) -> Config {
        Config {
            blockchain_interface: BlockchainInterfaceConfig {
                interface_type: "rpc".to_string(),
                network_type: "regtest".to_string(),
                url: url.map(str::to_string),
                rpc_user: user.map(str::to_string),
                rpc_password: password.map(str::to_string),
                rpc_import_addresses: import,
            },
            ..Default::default()
        }
    }

    #[test]
    fn sr_bchn_001_blockchain_factory_supports_woc_test_uaas_and_rpc() {
        assert!(blockchain_factory(&base_config("woc", None)).is_ok());
        assert!(blockchain_factory(&base_config("test", None)).is_ok());
        assert!(blockchain_factory(&base_config("uaas", Some("http://localhost:5010"))).is_ok());
        assert!(blockchain_factory(&rpc_config(
            Some("127.0.0.1:18443"),
            Some("u"),
            Some("p"),
            true
        ))
        .is_ok());
    }

    /// Only the node needs telling which addresses to follow, so only it
    /// supplies a watcher. A stray watcher on another backend would mean
    /// pointless RPC calls.
    #[test]
    fn sr_bchn_007_only_the_rpc_backend_supplies_an_address_watcher() {
        for interface_type in ["woc", "test"] {
            let backend = blockchain_factory(&base_config(interface_type, None)).unwrap();
            assert!(
                backend.address_watcher.is_none(),
                "{interface_type} should not supply a watcher"
            );
        }
        let backend =
            blockchain_factory(&base_config("uaas", Some("http://localhost:5010"))).unwrap();
        assert!(backend.address_watcher.is_none());

        let backend = blockchain_factory(&rpc_config(
            Some("127.0.0.1:18443"),
            Some("u"),
            Some("p"),
            true,
        ))
        .unwrap();
        assert!(backend.address_watcher.is_some());
    }

    /// An operator who manages the node's wallet can turn the import off.
    #[test]
    fn sr_bchn_007_rpc_import_addresses_false_supplies_no_watcher() {
        let backend = blockchain_factory(&rpc_config(
            Some("127.0.0.1:18443"),
            Some("u"),
            Some("p"),
            false,
        ))
        .unwrap();
        assert!(backend.address_watcher.is_none());
    }

    #[test]
    fn sr_bchn_005_rpc_interface_requires_address_and_credentials() {
        for (url, user, password, expected) in [
            (None, Some("u"), Some("p"), "url"),
            (Some("127.0.0.1:18443"), None, Some("p"), "rpc_user"),
            (Some("127.0.0.1:18443"), Some("u"), None, "rpc_password"),
        ] {
            let error = blockchain_factory(&rpc_config(url, user, password, true))
                .err()
                .expect("expected a missing-field error");
            assert!(error.contains(expected), "should name {expected}: {error}");
        }
    }

    #[test]
    fn sr_bchn_006_rpc_backend_can_be_built_for_regtest() {
        let config = rpc_config(Some("127.0.0.1:18443"), Some("u"), Some("p"), true);
        assert_eq!(config.get_network().unwrap(), Network::BSV_Regtest);
        assert!(blockchain_factory(&config).is_ok());
    }

    #[test]
    fn sr_bchn_001_blockchain_factory_rejects_unknown_interface_type() {
        let result = blockchain_factory(&base_config("unknown", None));
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("Unknown interface type"));
    }
}
