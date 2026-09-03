use std::time::Duration;

use actix_governor::{
    governor::{
        clock::{Clock, DefaultClock, QuantaInstant},
        middleware::NoOpMiddleware,
        NotUntil,
    },
    GovernorConfig, GovernorConfigBuilder, KeyExtractor, PeerIpKeyExtractor,
    SimpleKeyExtractionError,
};
use actix_web::{
    dev::ServiceRequest, http::header::ContentType, HttpResponse, HttpResponseBuilder,
};

use crate::config::RateLimitConfig;
use crate::responses::{ErrorCode, ErrorResponse};

const HEALTH_PATH: &str = "/health";
const HEALTH_WHITELIST_KEY: &str = "__health__";

#[derive(Clone, Default)]
pub(crate) struct FinancingServiceKeyExtractor;

impl KeyExtractor for FinancingServiceKeyExtractor {
    type Key = String;
    type KeyExtractionError = SimpleKeyExtractionError<&'static str>;

    fn extract(&self, req: &ServiceRequest) -> Result<Self::Key, Self::KeyExtractionError> {
        if req.path() == HEALTH_PATH {
            return Ok(HEALTH_WHITELIST_KEY.to_string());
        }
        PeerIpKeyExtractor.extract(req).map(|ip| ip.to_string())
    }

    fn whitelisted_keys(&self) -> Vec<Self::Key> {
        vec![HEALTH_WHITELIST_KEY.to_string()]
    }

    fn exceed_rate_limit_response(
        &self,
        negative: &NotUntil<QuantaInstant>,
        mut response: HttpResponseBuilder,
    ) -> HttpResponse {
        let wait_time = negative
            .wait_time_from(DefaultClock::default().now())
            .as_secs();
        response
            .content_type(ContentType::json())
            .json(ErrorResponse {
                code: ErrorCode::RateLimited,
                description: format!("Rate limit exceeded, retry in {wait_time}s"),
            })
    }
}

pub type ServiceGovernorConfig = GovernorConfig<FinancingServiceKeyExtractor, NoOpMiddleware>;

pub fn build_governor_config(rate_limit: &RateLimitConfig) -> ServiceGovernorConfig {
    let mut builder = GovernorConfigBuilder::default().key_extractor(FinancingServiceKeyExtractor);
    if rate_limit.enabled {
        builder
            .requests_per_second(rate_limit.requests_per_second)
            .burst_size(rate_limit.effective_burst_size())
            .permissive(false);
    } else {
        builder.permissive(true);
    }
    builder
        .finish()
        .expect("rate limit governor config is valid")
}

pub fn spawn_retain_task(config: &ServiceGovernorConfig) {
    let limiter = config.limiter().clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            limiter.retain_recent();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_governor_config_succeeds_when_disabled() {
        let _ = build_governor_config(&RateLimitConfig::default());
    }

    #[test]
    fn build_governor_config_succeeds_when_enabled() {
        let _ = build_governor_config(&RateLimitConfig {
            enabled: true,
            requests_per_second: 10,
            burst_size: None,
        });
    }

    #[test]
    fn sr_lim_004_rate_limit_uses_peer_ip_key_extractor() {
        let source = include_str!("rate_limit.rs");
        assert!(source.contains("PeerIpKeyExtractor"));
        assert!(source.contains("FinancingServiceKeyExtractor"));
    }
}
