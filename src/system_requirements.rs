//! Automated tests for requirements that are verified at repository or deployment level.

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn sr_nfr_001_ci_runs_cargo_fmt_check() {
        let workflow = std::fs::read_to_string(".github/workflows/rust.yml").unwrap();
        assert!(workflow.contains("cargo fmt --all -- --check"));
    }

    #[test]
    fn sr_nfr_002_ci_runs_cargo_clippy_with_warnings_denied() {
        let workflow = std::fs::read_to_string(".github/workflows/rust.yml").unwrap();
        assert!(workflow.contains("cargo clippy"));
        assert!(workflow.contains("-D warnings"));
    }

    #[test]
    fn sr_nfr_003_ci_runs_cargo_audit() {
        let workflow = std::fs::read_to_string(".github/workflows/rust.yml").unwrap();
        assert!(workflow.contains("cargo audit"));
    }

    #[test]
    fn sr_nfr_004_ci_runs_cargo_test() {
        let workflow = std::fs::read_to_string(".github/workflows/rust.yml").unwrap();
        assert!(workflow.contains("cargo test"));
    }

    #[test]
    fn sr_nfr_005_cargo_lock_is_committed() {
        assert!(Path::new("Cargo.lock").is_file());
    }

    #[test]
    fn sr_nfr_006_chain_gang_dependency_is_pinned_to_git_revision() {
        let manifest = std::fs::read_to_string("Cargo.toml").unwrap();
        assert!(manifest.contains("chain-gang"));
        assert!(manifest.contains("rev = "));
    }

    #[test]
    fn sr_nfr_007_main_returns_error_instead_of_panicking_on_config_failure() {
        let source = include_str!("main.rs");
        assert!(
            source.contains("load_config(\"FS_CONFIG\", \"data/financing-service.toml\").map_err(")
        );
    }

    #[test]
    fn sr_cfg_005_dockerfile_healthcheck_calls_health_endpoint() {
        let dockerfile = std::fs::read_to_string("Dockerfile").unwrap();
        assert!(dockerfile.contains("HEALTHCHECK"));
        assert!(dockerfile.contains("/health"));
    }

    #[test]
    fn sr_lim_001_hd_wallet_support_is_not_implemented() {
        let source = concat!(
            include_str!("client.rs"),
            include_str!("service.rs"),
            include_str!("config.rs"),
        )
        .to_ascii_lowercase();
        assert!(!source.contains("bip32"));
        assert!(!source.contains("hd_wallet"));
        assert!(!source.contains("hierarchical deterministic"));
    }

    #[test]
    fn sr_lim_002_https_is_not_provided_by_the_service_binary() {
        let manifest = std::fs::read_to_string("Cargo.toml").unwrap();
        assert!(!manifest.contains("rustls"));
        assert!(!manifest.contains("native-tls"));
        let main_source = include_str!("main.rs");
        assert!(!main_source.contains("rustls"));
    }

    #[test]
    fn sr_lim_005_same_client_concurrency_is_covered_by_service_tests() {
        let source = include_str!("service.rs");
        assert!(source.contains("concurrent_fund_requests_for_same_client_do_not_block"));
    }

    #[test]
    fn sr_tele_002_main_wraps_tracing_logger() {
        let source = include_str!("main.rs");
        assert!(source.contains("tracing_actix_web::TracingLogger"));
        assert!(source.contains("telemetry::init"));
    }

    #[test]
    fn sr_tele_004_telemetry_sets_service_resource_attributes() {
        let source = include_str!("telemetry.rs");
        assert!(source.contains("service.name"));
        assert!(source.contains("service.version"));
        assert!(source.contains("CARGO_PKG_VERSION"));
    }

    #[test]
    fn sr_tele_005_readme_and_configuration_document_opentelemetry() {
        let readme = std::fs::read_to_string("README.md").unwrap();
        assert!(readme.contains("OpenTelemetry"));
        assert!(readme.contains("Configuration.md#telemetry"));

        let configuration = std::fs::read_to_string("docs/Configuration.md").unwrap();
        assert!(configuration.contains("## [telemetry]"));
        assert!(configuration.contains("OTEL_EXPORTER_OTLP_ENDPOINT"));
    }

    #[test]
    fn sr_lim_006_opentelemetry_exports_traces_only() {
        let manifest = std::fs::read_to_string("Cargo.toml").unwrap();
        assert!(manifest.contains("opentelemetry-otlp"));
        assert!(manifest.contains("features = [\"grpc-tonic\", \"trace\"]"));
        assert!(!manifest.contains("metrics"));
    }
}
