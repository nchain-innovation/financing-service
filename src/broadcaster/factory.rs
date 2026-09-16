//! Choosing the broadcaster from configuration.
//!
//! The rule is one line long -- `[mapi_lite]` present means mapi-lite -- and
//! lives here rather than in `main` so that it, and the startup line that
//! announces the choice, can be tested without starting the service.

use std::sync::Arc;

use chain_gang::interface::BlockchainInterface;

use super::{mapi::MapiBroadcaster, woc::WocBroadcaster, TxBroadcaster};
use crate::config::Config;

/// The broadcaster `config` asks for.
///
/// With a `[mapi_lite]` section, funding transactions go to that mapi-lite
/// server. Without one, they go through `blockchain_interface` -- the object
/// `[blockchain_interface]` configured, which the service also reads from --
/// exactly as they did before mapi-lite was an option.
pub fn broadcaster_factory(
    config: &Config,
    blockchain_interface: Arc<dyn BlockchainInterface + Send + Sync>,
) -> Result<Arc<dyn TxBroadcaster>, String> {
    match &config.mapi_lite {
        Some(mapi_lite) => Ok(Arc::new(MapiBroadcaster::new(mapi_lite)?)),
        None => Ok(Arc::new(WocBroadcaster::new(
            blockchain_interface,
            &config.blockchain_interface.interface_type,
        ))),
    }
}

/// The startup log line saying where funding transactions will go.
///
/// Names the mapi-lite base URL when one is configured, and never the token.
pub fn describe_broadcaster(config: &Config) -> String {
    match &config.mapi_lite {
        Some(mapi_lite) => format!(
            "mapi-lite integration configured (base_url={}): funding transactions will be broadcast via mapi-lite",
            mapi_lite.base_url()
        ),
        None => format!(
            "mapi-lite not configured: funding transactions will be broadcast via the '{}' blockchain interface",
            config.blockchain_interface.interface_type
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcaster::MAPI_LITE;
    use crate::config::MapiLiteConfig;
    use crate::test_support::{test_blockchain_interface, test_config, unique_dynamic_config_path};

    fn with_mapi_lite(mut config: Config) -> Config {
        config.mapi_lite = Some(MapiLiteConfig::for_base_url("http://127.0.0.1:8080"));
        config
    }

    #[tokio::test]
    async fn sr_bchn_009_factory_selects_the_blockchain_interface_without_mapi_lite() {
        let config = test_config(&unique_dynamic_config_path());
        let blockchain = test_blockchain_interface(&config).await;

        let broadcaster = broadcaster_factory(&config, blockchain).expect("builds");

        // Named after the configured interface_type, which is what the
        // service used to broadcast through.
        assert_eq!(broadcaster.name(), "test");
        assert_ne!(broadcaster.name(), MAPI_LITE);
    }

    #[tokio::test]
    async fn sr_bchn_009_factory_selects_mapi_lite_when_configured() {
        let config = with_mapi_lite(test_config(&unique_dynamic_config_path()));
        let blockchain = test_blockchain_interface(&config).await;

        let broadcaster = broadcaster_factory(&config, blockchain).expect("builds");

        assert_eq!(broadcaster.name(), MAPI_LITE);
    }

    #[test]
    fn sr_bchn_010_describe_broadcaster_names_mapi_lite_and_its_url_when_configured() {
        let config = with_mapi_lite(test_config(&unique_dynamic_config_path()));
        let line = describe_broadcaster(&config);
        assert!(line.contains("mapi-lite integration configured"), "{line}");
        assert!(line.contains("base_url=http://127.0.0.1:8080"), "{line}");
        assert!(line.contains("broadcast via mapi-lite"), "{line}");
    }

    #[test]
    fn sr_bchn_010_describe_broadcaster_names_the_blockchain_interface_otherwise() {
        let config = test_config(&unique_dynamic_config_path());
        let line = describe_broadcaster(&config);
        assert!(line.contains("mapi-lite not configured"), "{line}");
        assert!(line.contains("'test' blockchain interface"), "{line}");
    }

    #[test]
    fn describe_broadcaster_never_includes_the_token() {
        let mut config = with_mapi_lite(test_config(&unique_dynamic_config_path()));
        config.mapi_lite.as_mut().unwrap().auth_token = Some("Bearer very-secret".to_string());
        assert!(!describe_broadcaster(&config).contains("very-secret"));
    }
}
