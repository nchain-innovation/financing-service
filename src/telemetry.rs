use std::env;

use log::Level;
use opentelemetry::{global, trace::TracerProvider as _, KeyValue};
use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    metrics::SdkMeterProvider, propagation::TraceContextPropagator, resource::Resource,
    trace::SdkTracerProvider,
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::config::TelemetryConfig;

/// Keeps the tracer and meter providers alive, and flushes spans and metrics
/// on shutdown.
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl TelemetryGuard {
    /// Whether a meter provider is installed, so the gauges' sampler is
    /// worth running (CS-608).
    pub fn metrics_enabled(&self) -> bool {
        self.meter_provider.is_some()
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.tracer_provider {
            if let Err(error) = provider.shutdown() {
                eprintln!("OpenTelemetry shutdown error: {error}");
            }
        }
        if let Some(provider) = &self.meter_provider {
            if let Err(error) = provider.shutdown() {
                eprintln!("OpenTelemetry metrics shutdown error: {error}");
            }
        }
    }
}

/// Initialise tracing, with optional OpenTelemetry trace export and optional
/// OTLP metrics export (CS-608). Each has its own switch; returns a guard when
/// either is on.
pub fn init(config: &TelemetryConfig, log_level: Level) -> Result<Option<TelemetryGuard>, String> {
    let filter = EnvFilter::new(log_level_filter(log_level));

    let tracer_provider = if config.is_enabled() {
        global::set_text_map_propagator(TraceContextPropagator::new());

        let exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(config.effective_otlp_endpoint())
            .build()
            .map_err(|e| format!("failed to create OTLP trace exporter: {e}"))?;

        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource(config))
            .build();

        global::set_tracer_provider(provider.clone());
        Some(provider)
    } else {
        None
    };
    let otel_layer = tracer_provider
        .as_ref()
        .map(|provider| OpenTelemetryLayer::new(provider.tracer("financing-service")));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer)
        .try_init()
        .map_err(|e| format!("failed to initialise tracing subscriber: {e}"))?;

    bridge_log_crate()?;
    if tracer_provider.is_some() {
        log::info!(
            "OpenTelemetry enabled (service.name={}, otlp.endpoint={})",
            config.effective_service_name(),
            redact_endpoint(&config.effective_otlp_endpoint())
        );
    }

    // After the subscriber, so the startup line is logged. Installed
    // globally: `global::meter` hands out the no-op meter until it is, which
    // is what makes every instrument free with export off.
    let meter_provider = build_meter_provider(config)?;
    if let Some(provider) = &meter_provider {
        global::set_meter_provider(provider.clone());
    }
    log::info!("{}", metrics_startup_line(config));

    if tracer_provider.is_none() && meter_provider.is_none() {
        return Ok(None);
    }
    Ok(Some(TelemetryGuard {
        tracer_provider,
        meter_provider,
    }))
}

/// `service.name` and `service.version`, on traces and metrics alike.
fn resource(config: &TelemetryConfig) -> Resource {
    Resource::builder()
        .with_attributes(vec![
            KeyValue::new("service.name", config.effective_service_name()),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        ])
        .build()
}

/// The OTLP metrics pipeline, or `None` when metrics export is off -- in which
/// case nothing is installed and `global::meter` stays the no-op meter.
///
/// The `PeriodicReader` exports at the SDK's default interval, which
/// `OTEL_METRIC_EXPORT_INTERVAL` (milliseconds) overrides.
pub fn build_meter_provider(config: &TelemetryConfig) -> Result<Option<SdkMeterProvider>, String> {
    if !config.metrics_enabled() {
        return Ok(None);
    }
    let exporter = MetricExporter::builder()
        .with_tonic()
        .with_endpoint(config.effective_metrics_otlp_endpoint())
        .build()
        .map_err(|e| format!("failed to create OTLP metric exporter: {e}"))?;
    Ok(Some(
        SdkMeterProvider::builder()
            .with_periodic_exporter(exporter)
            .with_resource(resource(config))
            .build(),
    ))
}

/// The one startup line saying whether metrics are exported, and where to.
pub fn metrics_startup_line(config: &TelemetryConfig) -> String {
    if config.metrics_enabled() {
        format!(
            "OpenTelemetry metrics export on (service.name={}, otlp.endpoint={})",
            config.effective_service_name(),
            redact_endpoint(&config.effective_metrics_otlp_endpoint())
        )
    } else {
        "OpenTelemetry metrics export off (enable with [telemetry] enabled = true or \
         OTEL_METRICS_EXPORTER=otlp)"
            .to_string()
    }
}

/// An endpoint fit for a log line: any `user:password@` and any query string
/// or fragment removed, so a credential carried in the URL is never logged.
fn redact_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();
    let Some(scheme_end) = endpoint.find("://").map(|i| i + 3) else {
        return endpoint;
    };
    let authority_end = endpoint[scheme_end..]
        .find('/')
        .map_or(endpoint.len(), |i| scheme_end + i);
    match endpoint[scheme_end..authority_end].rfind('@') {
        Some(at) => format!(
            "{}{}",
            &endpoint[..scheme_end],
            &endpoint[scheme_end + at + 1..]
        ),
        None => endpoint,
    }
}

fn bridge_log_crate() -> Result<(), String> {
    // `try_init()` above already installs the log -> tracing bridge via
    // tracing-subscriber's `tracing-log` feature, so this call is a no-op that
    // reports "logger already initialised". Ignore that and keep the bridge for
    // the case where the feature is ever turned off.
    let _ = tracing_log::LogTracer::init();
    Ok(())
}

fn log_level_filter(level: Level) -> &'static str {
    match level {
        Level::Error => "error",
        Level::Warn => "warn",
        Level::Info => "info",
        Level::Debug => "debug",
        Level::Trace => "trace",
    }
}

impl TelemetryConfig {
    pub fn is_enabled(&self) -> bool {
        if self.enabled {
            return true;
        }
        matches!(
            env::var("OTEL_TRACES_EXPORTER").as_deref(),
            Ok("otlp") | Ok("OTLP")
        )
    }

    /// Whether metrics are exported over OTLP (CS-608): `enabled = true`, or
    /// `OTEL_METRICS_EXPORTER=otlp`. Independent of `OTEL_TRACES_EXPORTER`.
    pub fn metrics_enabled(&self) -> bool {
        if self.enabled {
            return true;
        }
        matches!(
            env::var("OTEL_METRICS_EXPORTER").as_deref(),
            Ok("otlp") | Ok("OTLP")
        )
    }

    pub fn effective_service_name(&self) -> String {
        env::var("OTEL_SERVICE_NAME")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| self.service_name.clone().filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "financing-service".to_string())
    }

    pub fn effective_otlp_endpoint(&self) -> String {
        env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| self.otlp_endpoint.clone().filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "http://localhost:4317".to_string())
    }

    /// `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`, else as
    /// [`TelemetryConfig::effective_otlp_endpoint`] without the trace-specific
    /// variable.
    pub fn effective_metrics_otlp_endpoint(&self) -> String {
        env::var("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| self.otlp_endpoint.clone().filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "http://localhost:4317".to_string())
    }

    pub fn validate(&self) -> Result<(), String> {
        if (self.is_enabled() || self.metrics_enabled())
            && self.otlp_endpoint.as_deref().is_some_and(str::is_empty)
        {
            return Err("telemetry.otlp_endpoint cannot be empty when set".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::env_lock;

    #[test]
    fn telemetry_defaults_to_disabled_without_otel_exporter_env() {
        let _env = env_lock();
        unsafe { env::remove_var("OTEL_TRACES_EXPORTER") };
        let config = TelemetryConfig::default();
        assert!(!config.is_enabled());
    }

    #[test]
    fn otel_traces_exporter_env_enables_telemetry() {
        let _env = env_lock();
        unsafe { env::set_var("OTEL_TRACES_EXPORTER", "otlp") };
        let config = TelemetryConfig::default();
        assert!(config.is_enabled());
        unsafe { env::remove_var("OTEL_TRACES_EXPORTER") };
    }

    #[test]
    fn effective_service_name_prefers_otel_service_name_env() {
        let _env = env_lock();
        unsafe { env::set_var("OTEL_SERVICE_NAME", "from-env") };
        let config = TelemetryConfig {
            service_name: Some("from-config".to_string()),
            ..Default::default()
        };
        assert_eq!(config.effective_service_name(), "from-env");
        unsafe { env::remove_var("OTEL_SERVICE_NAME") };
    }

    #[test]
    fn effective_otlp_endpoint_uses_standard_env_vars() {
        let _env = env_lock();
        unsafe { env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4317") };
        let config = TelemetryConfig::default();
        assert_eq!(
            config.effective_otlp_endpoint(),
            "http://collector:4317".to_string()
        );
        unsafe { env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT") };
    }

    const METRICS_ENV: [&str; 3] = [
        "OTEL_METRICS_EXPORTER",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
    ];

    fn clear_metrics_env() {
        for name in METRICS_ENV {
            unsafe { env::remove_var(name) };
        }
    }

    /// Off by default, and off means nothing is built: no provider, so
    /// `global::meter` stays the no-op meter, and the startup line says so.
    #[test]
    fn sr_tele_006_metrics_export_is_off_by_default_and_installs_nothing() {
        let _env = env_lock();
        clear_metrics_env();
        let config = TelemetryConfig::default();
        assert!(!config.metrics_enabled());
        assert!(build_meter_provider(&config).unwrap().is_none());
        let line = metrics_startup_line(&config);
        assert!(line.contains("metrics export off"), "{line}");
    }

    /// Metrics have their own switch: the traces one does not turn them on.
    #[test]
    fn sr_tele_006_otel_traces_exporter_alone_does_not_enable_metrics() {
        let _env = env_lock();
        clear_metrics_env();
        unsafe { env::set_var("OTEL_TRACES_EXPORTER", "otlp") };
        let config = TelemetryConfig::default();
        assert!(config.is_enabled());
        assert!(!config.metrics_enabled());
        unsafe { env::remove_var("OTEL_TRACES_EXPORTER") };
    }

    #[test]
    fn sr_tele_006_otel_metrics_exporter_env_enables_metrics() {
        let _env = env_lock();
        clear_metrics_env();
        unsafe { env::set_var("OTEL_METRICS_EXPORTER", "otlp") };
        let config = TelemetryConfig::default();
        assert!(config.metrics_enabled());
        assert!(!config.is_enabled(), "traces keep their own switch");
        clear_metrics_env();
    }

    #[test]
    fn sr_tele_006_telemetry_enabled_turns_metrics_on() {
        let _env = env_lock();
        clear_metrics_env();
        let config = TelemetryConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(config.metrics_enabled());
    }

    /// Built inside a runtime because the tonic exporter needs one, as it
    /// does in `main`.
    #[tokio::test]
    async fn sr_tele_006_metrics_export_on_builds_a_provider_and_says_where() {
        let _env = env_lock();
        clear_metrics_env();
        unsafe { env::set_var("OTEL_METRICS_EXPORTER", "otlp") };
        unsafe {
            env::set_var(
                "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
                "http://user:secret@collector:4317/?token=abc",
            )
        };
        let config = TelemetryConfig::default();
        let provider = build_meter_provider(&config).unwrap();
        assert!(provider.is_some());
        let line = metrics_startup_line(&config);
        clear_metrics_env();
        assert!(line.contains("metrics export on"), "{line}");
        assert!(
            line.contains("otlp.endpoint=http://collector:4317/"),
            "{line}"
        );
        assert!(
            !line.contains("secret") && !line.contains("token"),
            "{line}"
        );
        let _ = provider.unwrap().shutdown();
    }

    #[test]
    fn sr_tele_006_metrics_endpoint_prefers_the_metrics_specific_variable() {
        let _env = env_lock();
        clear_metrics_env();
        let config = TelemetryConfig {
            otlp_endpoint: Some("http://from-config:4317".to_string()),
            ..Default::default()
        };
        assert_eq!(
            config.effective_metrics_otlp_endpoint(),
            "http://from-config:4317"
        );
        unsafe { env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://generic:4317") };
        assert_eq!(
            config.effective_metrics_otlp_endpoint(),
            "http://generic:4317"
        );
        unsafe { env::set_var("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", "http://metrics:4317") };
        assert_eq!(
            config.effective_metrics_otlp_endpoint(),
            "http://metrics:4317"
        );
        clear_metrics_env();
        assert_eq!(
            TelemetryConfig::default().effective_metrics_otlp_endpoint(),
            "http://localhost:4317"
        );
    }

    #[test]
    fn redact_endpoint_drops_credentials_and_query() {
        assert_eq!(
            redact_endpoint("http://localhost:4317"),
            "http://localhost:4317"
        );
        assert_eq!(
            redact_endpoint("https://u:p@host:4317/v1?key=1#x"),
            "https://host:4317/v1"
        );
        assert_eq!(redact_endpoint("host:4317"), "host:4317");
    }
}
