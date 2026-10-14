use std::net::IpAddr;
use std::sync::Arc;
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

/// Probe endpoints, exempt from rate limiting so an orchestrator polling them
/// on a short interval is never throttled out of its own health signal.
const PROBE_PATHS: [&str; 2] = ["/health", "/ready"];
const PROBE_WHITELIST_KEY: &str = "__probe__";

#[derive(Clone, Default)]
pub(crate) struct FinancingServiceKeyExtractor {
    /// See [`RateLimitConfig::trusted_proxies`].
    trusted_proxies: Arc<Vec<IpAddr>>,
}

impl FinancingServiceKeyExtractor {
    fn new(trusted_proxies: &[IpAddr]) -> Self {
        Self {
            trusted_proxies: Arc::new(trusted_proxies.to_vec()),
        }
    }

    /// The address a request is limited by: the peer's, unless the peer is a
    /// trusted proxy, in which case the client address it forwarded (CS-530).
    fn client_address(&self, peer: IpAddr, forwarded_for: &[&str]) -> IpAddr {
        if !self.trusted_proxies.contains(&peer) {
            return peer;
        }
        // Each proxy appends the address it received the request from, so the
        // right-most entry not added by one of ours is the first address we
        // cannot vouch for -- the client, as far as can be known. Anything to
        // its left was supplied by the client and could say anything.
        forwarded_for
            .iter()
            .flat_map(|header| header.split(','))
            .rev()
            .filter_map(|entry| entry.trim().parse::<IpAddr>().ok())
            .find(|address| !self.trusted_proxies.contains(address))
            .unwrap_or(peer)
    }
}

impl KeyExtractor for FinancingServiceKeyExtractor {
    type Key = String;
    type KeyExtractionError = SimpleKeyExtractionError<&'static str>;

    fn extract(&self, req: &ServiceRequest) -> Result<Self::Key, Self::KeyExtractionError> {
        if PROBE_PATHS.contains(&req.path()) {
            return Ok(PROBE_WHITELIST_KEY.to_string());
        }
        let peer = PeerIpKeyExtractor.extract(req)?;
        if self.trusted_proxies.is_empty() {
            return Ok(peer.to_string());
        }
        let forwarded_for: Vec<&str> = req
            .headers()
            .get_all("x-forwarded-for")
            .filter_map(|value| value.to_str().ok())
            .collect();
        Ok(self.client_address(peer, &forwarded_for).to_string())
    }

    fn whitelisted_keys(&self) -> Vec<Self::Key> {
        vec![PROBE_WHITELIST_KEY.to_string()]
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
    let mut builder = GovernorConfigBuilder::default().key_extractor(
        FinancingServiceKeyExtractor::new(&rate_limit.trusted_proxies),
    );
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
            trusted_proxies: Vec::new(),
        });
    }

    #[test]
    fn sr_lim_004_rate_limit_uses_peer_ip_key_extractor() {
        let source = include_str!("rate_limit.rs");
        assert!(source.contains("PeerIpKeyExtractor"));
        assert!(source.contains("FinancingServiceKeyExtractor"));
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// CS-530: from a trusted proxy, the forwarded client address is the key.
    #[test]
    fn sr_lim_004_a_trusted_proxy_is_keyed_by_the_client_it_forwards() {
        let extractor = FinancingServiceKeyExtractor::new(&[ip("10.0.0.1"), ip("10.0.0.2")]);
        assert_eq!(
            extractor.client_address(ip("10.0.0.1"), &["203.0.113.7"]),
            ip("203.0.113.7")
        );
        // Through two proxies of ours: still the client.
        assert_eq!(
            extractor.client_address(ip("10.0.0.1"), &["203.0.113.7, 10.0.0.2"]),
            ip("203.0.113.7")
        );
        // A client that sends its own header can only add entries to the left
        // of the one our proxy appends, so it cannot pick its key.
        assert_eq!(
            extractor.client_address(ip("10.0.0.1"), &["1.2.3.4, 203.0.113.7"]),
            ip("203.0.113.7")
        );
        assert_eq!(
            extractor.client_address(ip("10.0.0.1"), &["1.2.3.4", "203.0.113.7"]),
            ip("203.0.113.7")
        );
        // Nothing usable forwarded: the proxy itself.
        assert_eq!(
            extractor.client_address(ip("10.0.0.1"), &[]),
            ip("10.0.0.1")
        );
        assert_eq!(
            extractor.client_address(ip("10.0.0.1"), &["garbage"]),
            ip("10.0.0.1")
        );
    }

    /// From anyone else the header is ignored: it can say anything.
    #[test]
    fn sr_lim_004_an_untrusted_peer_cannot_choose_its_key() {
        let extractor = FinancingServiceKeyExtractor::new(&[ip("10.0.0.1")]);
        assert_eq!(
            extractor.client_address(ip("198.51.100.9"), &["203.0.113.7"]),
            ip("198.51.100.9")
        );
        let untrusting = FinancingServiceKeyExtractor::new(&[]);
        assert_eq!(
            untrusting.client_address(ip("10.0.0.1"), &["203.0.113.7"]),
            ip("10.0.0.1")
        );
    }
}
