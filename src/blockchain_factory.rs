use std::sync::Arc;

use chain_gang::interface::{
    BlockchainInterface, RpcInterface, TestInterface, UaaSInterface, WocInterface,
};

use crate::config::Config;

/// Takes a config and returns the appropriate configured object that implements BlockchainInterface
pub fn blockchain_factory(
    config: &Config,
) -> Result<Arc<dyn BlockchainInterface + Send + Sync>, String> {
    let network = config
        .get_network()
        .map_err(|_| "Unable to decode network from config".to_string())?;

    match config.blockchain_interface.interface_type.as_str() {
        "woc" => {
            let mut interface = WocInterface::new();
            interface.set_network(&network);
            Ok(Arc::new(interface))
        }
        "test" => {
            let mut interface = TestInterface::new();
            interface.set_network(&network);
            Ok(Arc::new(interface))
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
            Ok(Arc::new(interface))
        }
        "rpc" => {
            // A node we talk to directly over JSON-RPC. This is the only
            // interface that reaches regtest, since no public explorer serves
            // a private chain.
            let address = config
                .blockchain_interface
                .url
                .as_deref()
                .filter(|url| !url.is_empty())
                .ok_or_else(|| {
                    "Config blockchain interface url not found (the node's JSON-RPC host and port)."
                        .to_string()
                })?;
            let user = config
                .blockchain_interface
                .rpc_user
                .as_deref()
                .filter(|user| !user.is_empty())
                .ok_or_else(|| "Config blockchain interface rpc_user not found.".to_string())?;
            let password = config
                .blockchain_interface
                .rpc_password
                .as_deref()
                .filter(|password| !password.is_empty())
                .ok_or_else(|| "Config blockchain interface rpc_password not found.".to_string())?;
            Ok(Arc::new(RpcInterface::new(
                address, user, password, network,
            )))
        }
        other => Err(format!("Unknown interface type '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BlockchainInterfaceConfig;
    use chain_gang::network::Network;

    fn rpc_config(url: Option<&str>, user: Option<&str>, password: Option<&str>) -> Config {
        Config {
            blockchain_interface: BlockchainInterfaceConfig {
                interface_type: "rpc".to_string(),
                network_type: "regtest".to_string(),
                url: url.map(str::to_string),
                rpc_user: user.map(str::to_string),
                rpc_password: password.map(str::to_string),
            },
            ..Default::default()
        }
    }

    fn base_config(interface_type: &str, url: Option<&str>) -> Config {
        Config {
            blockchain_interface: BlockchainInterfaceConfig {
                interface_type: interface_type.to_string(),
                network_type: "testnet".to_string(),
                url: url.map(str::to_string),
                rpc_user: None,
                rpc_password: None,
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
            Some("rpcuser"),
            Some("rpcpassword")
        ))
        .is_ok());
    }

    /// The node's address and credentials are all required, and a missing one
    /// must be reported rather than producing an interface that fails on its
    /// first call.
    #[test]
    fn sr_bchn_005_rpc_interface_requires_address_and_credentials() {
        for (url, user, password, expected) in [
            (None, Some("u"), Some("p"), "url"),
            (Some("127.0.0.1:18443"), None, Some("p"), "rpc_user"),
            (Some("127.0.0.1:18443"), Some("u"), None, "rpc_password"),
        ] {
            let result = blockchain_factory(&rpc_config(url, user, password));
            let error = result.err().expect("expected a missing-field error");
            assert!(
                error.contains(expected),
                "error should name {expected}, got: {error}"
            );
        }
    }

    /// regtest is reachable only through the node, so this pairing is the point
    /// of the interface.
    #[test]
    fn sr_bchn_006_rpc_interface_can_be_built_for_regtest() {
        let config = rpc_config(Some("127.0.0.1:18443"), Some("u"), Some("p"));
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
