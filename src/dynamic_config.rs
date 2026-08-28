use crate::config::{ClientConfig, Config};

use serde::{Deserialize, Serialize};

// Represents the service's dynamically configurable elements

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct FileContents {
    pub clients: Vec<ClientConfig>,
}

pub struct DynamicConfig {
    filename: String,
    pub contents: FileContents,
}

fn read_dynamic_config(filename: &str) -> Result<FileContents, String> {
    let content = std::fs::read_to_string(filename).map_err(|e| e.to_string())?;
    let config = toml::from_str(&content).map_err(|e| e.to_string())?;
    Ok(config)
}

fn save_dynamic_config(filename: &str, file_contents: &FileContents) -> Result<(), String> {
    let content =
        toml::to_string(file_contents).map_err(|e| format!("Failed to serialize config: {e}"))?;
    std::fs::write(filename, content).map_err(|e| format!("Failed to write config: {e}"))?;
    Ok(())
}

impl DynamicConfig {
    pub fn new(config: &Config) -> Self {
        let filename = config.dynamic_config.filename.clone();

        let contents: FileContents = match read_dynamic_config(&filename) {
            Ok(contents) => contents,
            Err(e) => {
                println!("Dynamic Config Error {:?} in {}", e, filename);
                FileContents::default()
            }
        };

        DynamicConfig { filename, contents }
    }

    pub fn add(&mut self, new_client: &ClientConfig) -> Result<(), String> {
        self.contents.clients.push(new_client.clone());
        self.save()
    }

    pub fn remove(&mut self, client_id: &str) -> Result<(), String> {
        if let Some(index) = self
            .contents
            .clients
            .iter()
            .position(|c| c.client_id == client_id)
        {
            self.contents.clients.remove(index);
            self.save()
        } else {
            Ok(())
        }
    }

    fn save(&self) -> Result<(), String> {
        save_dynamic_config(&self.filename, &self.contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DynamicConfigConfig};

    fn temp_dynamic_config() -> (String, DynamicConfig) {
        let path = std::env::temp_dir()
            .join(format!(
                "financing-service-dynamic-{}-{}.toml",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .to_string_lossy()
            .into_owned();
        let config = Config {
            dynamic_config: DynamicConfigConfig {
                filename: path.clone(),
            },
            ..Default::default()
        };
        (path, DynamicConfig::new(&config))
    }

    #[test]
    fn sr_clnt_004_and_sr_nfr_008_add_client_persists_to_dynamic_config_file() {
        let (path, mut dynamic_config) = temp_dynamic_config();
        let client = ClientConfig {
            client_id: "runtime-client".to_string(),
            wif_key: "env:FS_RUNTIME_WIF".to_string(),
            api_key: Some("env:FS_RUNTIME_API_KEY".to_string()),
        };
        dynamic_config.add(&client).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("runtime-client"));
        assert!(saved.contains("env:FS_RUNTIME_WIF"));
        assert!(saved.contains("env:FS_RUNTIME_API_KEY"));
    }

    #[test]
    fn sr_nfr_008_remove_client_updates_dynamic_config_file() {
        let (path, mut dynamic_config) = temp_dynamic_config();
        let client = ClientConfig {
            client_id: "to-remove".to_string(),
            wif_key: "env:FS_REMOVE_WIF".to_string(),
            api_key: None,
        };
        dynamic_config.add(&client).unwrap();
        dynamic_config.remove("to-remove").unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("to-remove"));
    }
}
