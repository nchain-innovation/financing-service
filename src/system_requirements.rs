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
    fn sr_nfr_007_main_returns_error_instead_of_panicking_on_config_failure() {
        let source = include_str!("main.rs");
        assert!(
            source.contains("load_config(\"FS_CONFIG\", \"data/financing-service.toml\").map_err(")
        );
    }

    /// The line an operator reads to learn where funding transactions go.
    #[test]
    fn sr_bchn_010_main_logs_which_broadcaster_is_in_use() {
        let source = include_str!("main.rs");
        assert!(source.contains("describe_broadcaster(&config)"));
    }

    /// uls-client and uls-core share types; two revisions would give two
    /// incompatible sets. Both pins in the manifest must name one commit.
    #[test]
    fn sr_bchn_009_uls_client_and_uls_core_are_pinned_to_one_mapi_lite_revision() {
        let manifest = std::fs::read_to_string("Cargo.toml").unwrap();
        let revs: Vec<&str> = manifest
            .lines()
            .filter(|line| line.starts_with("uls-client") || line.starts_with("uls-core"))
            .map(|line| {
                let start = line.find("rev = \"").expect("a rev pin") + "rev = \"".len();
                let end = line[start..].find('"').expect("closing quote") + start;
                &line[start..end]
            })
            .collect();
        assert_eq!(
            revs.len(),
            2,
            "expected uls-client and uls-core pins: {revs:?}"
        );
        assert_eq!(
            revs[0], revs[1],
            "uls-client and uls-core pin different revs"
        );
        assert!(
            manifest.contains("git = \"ssh://git@github.com/nchain-innovation/mapi-lite.git\""),
            "cargo needs the ssh:// URL form for the private mapi-lite repo"
        );
    }

    /// The private mapi-lite repo is fetched by the system git so the
    /// developer's (or CI's) ssh-agent authenticates it.
    #[test]
    fn sr_nfr_009_cargo_fetches_git_dependencies_with_the_system_git() {
        let cargo_config = std::fs::read_to_string(".cargo/config.toml").unwrap();
        assert!(cargo_config.contains("git-fetch-with-cli = true"));
    }

    #[test]
    fn sr_nfr_009_ci_loads_the_mapi_lite_deploy_key_before_building() {
        let workflow = std::fs::read_to_string(".github/workflows/rust.yml").unwrap();
        assert!(workflow.contains("MAPI_LITE_DEPLOY_KEY"));
        assert!(workflow.contains("webfactory/ssh-agent"));
    }

    #[test]
    fn sr_nfr_009_docker_build_mounts_ssh_for_the_dependency_fetch() {
        let dockerfile = std::fs::read_to_string("Dockerfile").unwrap();
        assert!(dockerfile.contains("--mount=type=ssh"));
        let build = std::fs::read_to_string("build.sh").unwrap();
        assert!(build.contains("--ssh default"));
    }

    #[test]
    fn sr_bchn_009_configuration_and_readme_document_mapi_lite() {
        let configuration = std::fs::read_to_string("docs/Configuration.md").unwrap();
        assert!(configuration.contains("## [mapi_lite]"));
        assert!(configuration.contains("FS_MAPI_LITE_AUTH_TOKEN"));
        let readme = std::fs::read_to_string("README.md").unwrap();
        assert!(readme.contains("Configuration.md#mapi_lite"));
        let endpoints = std::fs::read_to_string("docs/SupportedEndpoints.md").unwrap();
        assert!(endpoints.contains("\"unhealthy\""));
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

    /// The ticket's first point was that the documentation never said whether
    /// unconfirmed outputs could be used as funds, so a reader had to guess --
    /// and guessed wrong. The behaviour is pinned by a test in `client`; this
    /// pins that it is also written down, which is the half a caller can see.
    #[test]
    fn sr_fund_019_endpoints_document_unconfirmed_eligibility() {
        let endpoints = std::fs::read_to_string("docs/SupportedEndpoints.md").unwrap();
        assert!(
            endpoints.contains("Unconfirmed outputs are spendable"),
            "the balance endpoint must state whether unconfirmed funds can be spent"
        );
        assert!(
            endpoints.contains("derived from the unspent set"),
            "and that the figures come from the unspent set rather than a separate query"
        );
    }

    #[test]
    fn sr_lim_006_opentelemetry_exports_traces_only() {
        let manifest = std::fs::read_to_string("Cargo.toml").unwrap();
        assert!(manifest.contains("opentelemetry-otlp"));
        assert!(manifest.contains("features = [\"grpc-tonic\", \"trace\"]"));
        assert!(!manifest.contains("metrics"));
    }
}
