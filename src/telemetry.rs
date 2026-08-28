use std::env;

use log::Level;
use opentelemetry::{global, trace::TracerProvider as _, KeyValue};
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    propagation::TraceContextPropagator, resource::Resource, trace::SdkTracerProvider,
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::config::TelemetryConfig;

/// Keeps the tracer provider alive and flushes spans on shutdown.
pub struct TelemetryGuard {
    provider: SdkTracerProvider,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Err(error) = self.provider.shutdown() {
            eprintln!("OpenTelemetry shutdown error: {error}");
        }
    }
}

/// Initialise tracing (and optional OpenTelemetry export). Returns a guard when export is enabled.
pub fn init(config: &TelemetryConfig, log_level: Level) -> Result<Option<TelemetryGuard>, String> {
    let filter = EnvFilter::new(log_level_filter(log_level));

    if !config.is_enabled() {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .map_err(|e| format!("failed to initialise tracing subscriber: {e}"))?;
        bridge_log_crate()?;
        return Ok(None);
    }

    let service_name = config.effective_service_name();
    let endpoint = config.effective_otlp_endpoint();

    global::set_text_map_propagator(TraceContextPropagator::new());

    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()
        .map_err(|e| format!("failed to create OTLP trace exporter: {e}"))?;

    let resource = Resource::builder()
        .with_attributes(vec![
            KeyValue::new("service.name", service_name.clone()),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        ])
        .build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    global::set_tracer_provider(provider.clone());
    let tracer = provider.tracer("financing-service");
    let otel_layer = OpenTelemetryLayer::new(tracer);

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer)
        .try_init()
        .map_err(|e| format!("failed to initialise tracing subscriber: {e}"))?;

    bridge_log_crate()?;
    log::info!(
        "OpenTelemetry enabled (service.name={}, otlp.endpoint={})",
        service_name,
        endpoint
    );

    Ok(Some(TelemetryGuard { provider }))
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

    pub fn validate(&self) -> Result<(), String> {
        if self.is_enabled() && self.otlp_endpoint.as_deref().is_some_and(str::is_empty) {
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
}
