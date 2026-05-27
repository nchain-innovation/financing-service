use std::env;

pub const ENV_PREFIX: &str = "env:";

/// Resolve a config value, substituting `env:VAR_NAME` from the process environment.
pub fn resolve_secret(value: &str) -> Result<String, String> {
    if let Some(var_name) = value.strip_prefix(ENV_PREFIX) {
        if var_name.is_empty() {
            return Err(
                "Secret reference 'env:' is missing an environment variable name".to_string(),
            );
        }
        env::var(var_name).map_err(|_| {
            format!("Environment variable '{var_name}' is not set (required by config secret reference)")
        })
    } else {
        Ok(value.to_string())
    }
}

pub fn secret_reference(var_name: &str) -> String {
    format!("{ENV_PREFIX}{var_name}")
}

pub fn is_secret_reference(value: &str) -> bool {
    value.starts_with(ENV_PREFIX)
}

pub fn validate_env_var_name(name: &str) -> Result<(), String> {
    let Some(first) = name.chars().next() else {
        return Err("Environment variable name cannot be empty".to_string());
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!(
            "Invalid environment variable name '{name}': must start with a letter or underscore"
        ));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(format!(
            "Invalid environment variable name '{name}': use only letters, numbers, and underscores"
        ));
    }
    Ok(())
}

pub fn warn_plaintext_secrets(config: &crate::config::Config) {
    for field in plaintext_secret_fields(config) {
        if field == "web_interface.admin_api_key" {
            log::warn!(
                "web_interface.admin_api_key is stored as plaintext in config; use env:VAR_NAME or FS_ADMIN_API_KEY instead"
            );
        } else if let Some(client_id) = field.strip_prefix("client.").and_then(|rest| {
            rest.strip_suffix(".wif_key")
                .or_else(|| rest.strip_suffix(".api_key"))
        }) {
            let kind = if field.ends_with(".wif_key") {
                "wif_key"
            } else {
                "api_key"
            };
            log::warn!(
                "Client '{client_id}' {kind} is stored as plaintext in config; use env:VAR_NAME instead"
            );
        }
    }
}

pub fn warn_plaintext_client_secrets(client_id: &str, wif_key: &str, api_key: Option<&str>) {
    if !is_secret_reference(wif_key) {
        log::warn!(
            "Client '{client_id}' wif_key is stored as plaintext in config; use env:VAR_NAME instead"
        );
    }
    if api_key.is_some_and(|key| !key.is_empty() && !is_secret_reference(key)) {
        log::warn!(
            "Client '{client_id}' api_key is stored as plaintext in config; use env:VAR_NAME instead"
        );
    }
}

/// Return config fields stored as plaintext secrets rather than `env:VAR` references.
pub fn plaintext_secret_fields(config: &crate::config::Config) -> Vec<String> {
    let mut fields = Vec::new();
    if config
        .web_interface
        .admin_api_key
        .as_deref()
        .is_some_and(|key| !is_secret_reference(key))
    {
        fields.push("web_interface.admin_api_key".to_string());
    }
    if let Some(clients) = &config.client {
        for client in clients {
            if !is_secret_reference(&client.wif_key) {
                fields.push(format!("client.{}.wif_key", client.client_id));
            }
            if client
                .api_key
                .as_deref()
                .is_some_and(|key| !key.is_empty() && !is_secret_reference(key))
            {
                fields.push(format!("client.{}.api_key", client.client_id));
            }
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_secret_returns_literal_value() {
        assert_eq!(resolve_secret("literal-key").unwrap(), "literal-key");
    }

    #[test]
    fn resolve_secret_reads_environment_variable() {
        unsafe { env::set_var("FS_TEST_SECRET", "from-env") };
        assert_eq!(resolve_secret("env:FS_TEST_SECRET").unwrap(), "from-env");
    }

    #[test]
    fn resolve_secret_errors_when_env_var_missing() {
        assert!(resolve_secret("env:FS_DEFINITELY_MISSING_SECRET").is_err());
    }

    #[test]
    fn validate_env_var_name_rejects_invalid_names() {
        assert!(validate_env_var_name("VALID_NAME_1").is_ok());
        assert!(validate_env_var_name("1INVALID").is_err());
        assert!(validate_env_var_name("bad-name").is_err());
    }

    #[test]
    fn sr_sec_009_plaintext_secret_fields_detects_literal_secrets() {
        let config = crate::config::Config {
            web_interface: crate::config::WebInterfaceConfig {
                admin_api_key: Some("literal-admin".to_string()),
                ..Default::default()
            },
            client: Some(vec![crate::config::ClientConfig {
                client_id: "id1".to_string(),
                wif_key: "literal-wif".to_string(),
                api_key: Some("literal-api-key".to_string()),
            }]),
            ..Default::default()
        };
        let fields = plaintext_secret_fields(&config);
        assert!(fields.contains(&"web_interface.admin_api_key".to_string()));
        assert!(fields.contains(&"client.id1.wif_key".to_string()));
        assert!(fields.contains(&"client.id1.api_key".to_string()));
    }
}
