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

fn read_dynamic_config(filename: &str) -> std::io::Result<FileContents> {
    let content = std::fs::read_to_string(filename)?;
    Ok(toml::from_str(&content)?)
}

fn save_dynamic_config(filename: &str, file_contents: &FileContents) -> std::io::Result<()> {
    let content = toml::to_string(file_contents).unwrap();
    std::fs::write(filename, content)?;
    Ok(())
}

impl DynamicConfig {
    pub fn new(config: &Config) -> Self {
        let filename = config.dynamic_config.filename.clone();

        let contents: FileContents = match read_dynamic_config(&filename) {
            Ok(contents) => contents,
            Err(e) => {
                println!("Dynamic Config Error {:?} in {}", e, &filename);
                FileContents::default()
            }
        };

        DynamicConfig { 
            filename, 
            contents,
        }
    }

    pub fn add(&mut self, new_client: &ClientConfig) {
        self.contents.clients.push(new_client.clone());
        self.save();
    }

    pub fn remove(&mut self, client_id: &str) {
        if let Some(index) = self.contents.clients.iter().position(|c| c.client_id == client_id) {
            self.contents.clients.remove(index);
            self.save();
        }
    }

    fn save(&self) {
        save_dynamic_config(&self.filename, &self.contents).unwrap();
    }
}
