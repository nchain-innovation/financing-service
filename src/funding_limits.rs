//! Per-client limits on what `POST /fund` pays out (CS-530).
//!
//! Without them, one request -- or a quick run of them -- could pay out a
//! client's whole wallet, so a leaked client API key was worth everything in
//! that wallet at once. Both limits are optional and off by default: what is a
//! sensible ceiling depends entirely on what a deployment's clients fund.
//!
//! * `max_satoshis_per_request` bounds one request's payout, `satoshi` times
//!   `no_of_outpoints`, fees aside.
//! * `max_satoshis_per_period` bounds what one client is paid out within any
//!   `period_seconds` window. A request's amount is held against the window
//!   before it is funded, so two concurrent requests cannot both fit under
//!   it, and given back only when nothing was broadcast. Held amounts live in
//!   memory, so the window restarts with the service.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// `[funding_limits]`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct FundingLimitsConfig {
    /// Largest payout one `POST /fund` may ask for, in satoshis. Unset: no
    /// limit beyond the 21 million BSV every request is held to.
    #[serde(default)]
    pub max_satoshis_per_request: Option<u64>,
    /// Largest total payout one client may receive within `period_seconds`.
    /// Unset: no limit.
    #[serde(default)]
    pub max_satoshis_per_period: Option<u64>,
    /// The window `max_satoshis_per_period` applies to. Default one hour.
    #[serde(default = "default_period_seconds")]
    pub period_seconds: u64,
}

fn default_period_seconds() -> u64 {
    3600
}

impl Default for FundingLimitsConfig {
    fn default() -> Self {
        Self {
            max_satoshis_per_request: None,
            max_satoshis_per_period: None,
            period_seconds: default_period_seconds(),
        }
    }
}

impl FundingLimitsConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_satoshis_per_period.is_some() && self.period_seconds == 0 {
            return Err("funding_limits.period_seconds must be at least 1".to_string());
        }
        if self.max_satoshis_per_request == Some(0) || self.max_satoshis_per_period == Some(0) {
            return Err(
                "a funding_limits maximum of 0 would refuse every request; leave it unset for \
                 no limit"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn period(&self) -> Duration {
        Duration::from_secs(self.period_seconds)
    }
}

/// An amount held against a client's window until the request's outcome is
/// known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    client_id: String,
    id: u64,
}

/// Why a request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitExceeded {
    /// Over `max_satoshis_per_request`: no retry will succeed.
    PerRequest { requested: u64, limit: u64 },
    /// Over `max_satoshis_per_period`: retry once `retry_after` has passed.
    PerPeriod {
        requested: u64,
        limit: u64,
        held: u64,
        retry_after: Duration,
    },
}

impl LimitExceeded {
    pub fn description(&self) -> String {
        match self {
            LimitExceeded::PerRequest { requested, limit } => format!(
                "This request pays out {requested} satoshis, over this service's limit of \
                 {limit} per request"
            ),
            LimitExceeded::PerPeriod {
                requested,
                limit,
                held,
                retry_after,
            } => format!(
                "This request pays out {requested} satoshis, and this client has been paid \
                 {held} of its {limit} for the current period; retry in {}s",
                retry_after.as_secs().max(1)
            ),
        }
    }
}

/// What each client has been paid out within the current window.
#[derive(Debug)]
pub struct FundingLimits {
    config: FundingLimitsConfig,
    windows: Mutex<Windows>,
}

#[derive(Debug, Default)]
struct Windows {
    next_id: u64,
    /// Per client: `(hold id, when, satoshis)`, oldest first.
    by_client: HashMap<String, VecDeque<(u64, Instant, u64)>>,
}

impl FundingLimits {
    pub fn new(config: FundingLimitsConfig) -> Self {
        Self {
            config,
            windows: Mutex::new(Windows::default()),
        }
    }

    /// Checks `requested` satoshis against both limits and, when there is a
    /// per-period limit, holds it against the client's window. `Ok(None)`
    /// when there is nothing to hold.
    pub fn check_and_hold(
        &self,
        client_id: &str,
        requested: u64,
    ) -> Result<Option<Hold>, LimitExceeded> {
        if let Some(limit) = self.config.max_satoshis_per_request {
            if requested > limit {
                return Err(LimitExceeded::PerRequest { requested, limit });
            }
        }
        let Some(limit) = self.config.max_satoshis_per_period else {
            return Ok(None);
        };
        let period = self.config.period();
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("funding limits lock");
        let id = windows.next_id;
        windows.next_id += 1;
        let window = windows.by_client.entry(client_id.to_string()).or_default();
        while window
            .front()
            .is_some_and(|(_, at, _)| now.duration_since(*at) >= period)
        {
            window.pop_front();
        }
        let held: u64 = window.iter().map(|(_, _, amount)| amount).sum();
        if held.saturating_add(requested) > limit {
            // Waiting until the oldest amounts leave the window is the soonest
            // a retry could fit. If even an empty window could not take this
            // request, waiting a whole period is the honest answer.
            let mut freed = 0u64;
            let mut retry_after = period;
            for (_, at, amount) in window.iter() {
                freed = freed.saturating_add(*amount);
                if held - freed + requested <= limit {
                    retry_after = period.saturating_sub(now.duration_since(*at));
                    break;
                }
            }
            return Err(LimitExceeded::PerPeriod {
                requested,
                limit,
                held,
                retry_after,
            });
        }
        window.push_back((id, now, requested));
        Ok(Some(Hold {
            client_id: client_id.to_string(),
            id,
        }))
    }

    /// Gives a held amount back: the request it was held for broadcast
    /// nothing.
    pub fn release(&self, hold: &Hold) {
        let mut windows = self.windows.lock().expect("funding limits lock");
        if let Some(window) = windows.by_client.get_mut(&hold.client_id) {
            window.retain(|(id, _, _)| *id != hold.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(per_request: Option<u64>, per_period: Option<u64>, seconds: u64) -> FundingLimits {
        FundingLimits::new(FundingLimitsConfig {
            max_satoshis_per_request: per_request,
            max_satoshis_per_period: per_period,
            period_seconds: seconds,
        })
    }

    #[test]
    fn sr_sec_030_off_by_default() {
        let limits = FundingLimits::new(FundingLimitsConfig::default());
        assert_eq!(limits.check_and_hold("c", u64::MAX), Ok(None));
    }

    #[test]
    fn sr_sec_030_a_request_over_the_per_request_limit_is_refused() {
        let limits = limits(Some(1000), None, 3600);
        assert_eq!(limits.check_and_hold("c", 1000), Ok(None));
        assert_eq!(
            limits.check_and_hold("c", 1001),
            Err(LimitExceeded::PerRequest {
                requested: 1001,
                limit: 1000
            })
        );
    }

    #[test]
    fn sr_sec_030_the_period_limit_is_per_client_and_counts_what_was_held() {
        let limits = limits(None, Some(1000), 3600);
        assert!(limits.check_and_hold("a", 600).unwrap().is_some());
        assert!(limits.check_and_hold("a", 400).unwrap().is_some());
        let refused = limits.check_and_hold("a", 1).unwrap_err();
        assert!(
            matches!(refused, LimitExceeded::PerPeriod { held: 1000, .. }),
            "{refused:?}"
        );
        // Another client's window is its own.
        assert!(limits.check_and_hold("b", 1000).unwrap().is_some());
    }

    /// A request that broadcast nothing gives its amount back.
    #[test]
    fn sr_sec_030_a_released_hold_frees_its_amount() {
        let limits = limits(None, Some(1000), 3600);
        let hold = limits.check_and_hold("a", 1000).unwrap().unwrap();
        assert!(limits.check_and_hold("a", 1).is_err());
        limits.release(&hold);
        assert!(limits.check_and_hold("a", 1000).is_ok());
    }

    #[test]
    fn sr_sec_030_amounts_leave_the_window_when_it_passes() {
        let limits = limits(None, Some(1000), 1);
        limits.check_and_hold("a", 1000).unwrap();
        let LimitExceeded::PerPeriod { retry_after, .. } =
            limits.check_and_hold("a", 1).unwrap_err()
        else {
            panic!("expected the period limit");
        };
        assert!(retry_after <= Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(1050));
        assert!(limits.check_and_hold("a", 1000).is_ok());
    }

    #[test]
    fn sr_sec_030_a_zero_limit_or_period_is_a_configuration_error() {
        let config = |r, p, s| FundingLimitsConfig {
            max_satoshis_per_request: r,
            max_satoshis_per_period: p,
            period_seconds: s,
        };
        assert!(config(Some(0), None, 3600).validate().is_err());
        assert!(config(None, Some(0), 3600).validate().is_err());
        assert!(config(None, Some(10), 0).validate().is_err());
        assert!(config(Some(10), Some(10), 3600).validate().is_ok());
    }
}
