// src/strategies/storage.rs

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub id: String,
    pub name: String,
    pub symbol: String,
    pub code: String,
    pub enabled: bool,
}

pub struct StrategyStorage {
    base_path: PathBuf,
}

impl StrategyStorage {
    pub fn new(base_path: &str) -> Result<Self> {
        let path = PathBuf::from(base_path);
        if !path.exists() {
            fs::create_dir_all(&path)?;
        }
        Ok(Self { base_path: path })
    }

    pub fn create(&self, strategy: Strategy) -> Result<()> {
        let file_path = self.base_path.join(format!("{}.json", strategy.id));
        
        if file_path.exists() {
            anyhow::bail!("Strategy already exists");
        }
        
        let json = serde_json::to_string_pretty(&strategy)?;
        fs::write(&file_path, json)?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Strategy> {
        let file_path = self.base_path.join(format!("{}.json", id));
        let json = fs::read_to_string(&file_path)?;
        let strategy: Strategy = serde_json::from_str(&json)?;
        Ok(strategy)
    }

    pub fn save(&self, strategy: &Strategy) -> Result<()> {
        let file_path = self.base_path.join(format!("{}.json", strategy.id));
        let json = serde_json::to_string_pretty(strategy)?;
        fs::write(&file_path, json)?;
        Ok(())
    }

    pub fn update_code(&self, id: &str, new_code: String) -> Result<()> {
        let mut strategy = self.load(id)?;
        strategy.code = new_code;
        self.save(&strategy)?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let file_path = self.base_path.join(format!("{}.json", id));
        fs::remove_file(&file_path)?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Strategy>> {
        let mut strategies = Vec::new();
        
        for entry in fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let json = fs::read_to_string(&path)?;
                if let Ok(strategy) = serde_json::from_str::<Strategy>(&json) {
                    strategies.push(strategy);
                }
            }
        }
        
        Ok(strategies)
    }
}