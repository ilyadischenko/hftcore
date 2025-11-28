// src/strategies/storage.rs

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use anyhow::{Result, Context};
use serde::Serialize;

// ═══════════════════════════════════════════════════════════
// ТИПЫ
// ═══════════════════════════════════════════════════════════

/// Информация о стратегии (генерируется на лету)
#[derive(Debug, Clone, Serialize)]
pub struct StrategyInfo {
    pub id: String,
    pub compiled: bool,
}

/// Результат компиляции
#[derive(Debug)]
pub struct CompilationResult {
    pub success: bool,
    pub lib_path: Option<PathBuf>,
    pub output: String,
    pub errors: Vec<String>,
}

// ═══════════════════════════════════════════════════════════
// STORAGE
// ═══════════════════════════════════════════════════════════

pub struct StrategyStorage {
    base_path: PathBuf,
    templates_path: PathBuf,
}

impl StrategyStorage {
    pub fn new(base_path: &str) -> Result<Self> {
        let base = PathBuf::from(base_path);
        let templates = PathBuf::from("copy_into_strategies");
        
        if !base.exists() {
            fs::create_dir_all(&base)?;
        }
        
        // Проверяем шаблоны
        if !templates.join("types.rs").exists() {
            anyhow::bail!("Missing: copy_into_strategies/types.rs");
        }
        if !templates.join("Cargo.toml").exists() {
            anyhow::bail!("Missing: copy_into_strategies/Cargo.toml");
        }
        
        tracing::info!("✅ StrategyStorage initialized at {:?}", base);
        
        Ok(Self { base_path: base, templates_path: templates })
    }
    
    // ═══════════════════════════════════════════════════════════
    // CRUD
    // ═══════════════════════════════════════════════════════════
    
    /// Создать стратегию
    pub fn create(&self, id: &str, code: &str) -> Result<()> {
        let dir = self.base_path.join(id);
        
        if dir.exists() {
            anyhow::bail!("Strategy '{}' already exists", id);
        }
        
        fs::create_dir_all(dir.join("src"))?;
        
        // Копируем шаблоны
        self.copy_cargo_toml(&dir, id)?;
        self.copy_types(&dir)?;
        self.save_code(&dir, code)?;
        
        tracing::info!("✅ Strategy '{}' created", id);
        Ok(())
    }
    
    /// Получить код стратегии
    pub fn get_code(&self, id: &str) -> Result<String> {
        let dir = self.base_path.join(id);
        if !dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        self.load_code(&dir)
    }
    
    /// Обновить код стратегии
    pub fn update_code(&self, id: &str, code: &str) -> Result<()> {
        let dir = self.base_path.join(id);
        if !dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        // Проверяем types.rs
        if !dir.join("src/types.rs").exists() {
            self.copy_types(&dir)?;
        }
        
        self.save_code(&dir, code)?;
        tracing::info!("✏️ Strategy '{}' code updated", id);
        Ok(())
    }
    
    /// Удалить стратегию
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.base_path.join(id);
        if !dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        fs::remove_dir_all(&dir)?;
        tracing::info!("🗑️ Strategy '{}' deleted", id);
        Ok(())
    }
    
    /// Список всех стратегий
    pub fn list(&self) -> Result<Vec<StrategyInfo>> {
        let mut result = Vec::new();
        
        for entry in fs::read_dir(&self.base_path)? {
            let entry = entry?;
            if entry.path().is_dir() {
                if let Some(id) = entry.file_name().to_str() {
                    let compiled = self.get_lib_path(id).is_ok();
                    result.push(StrategyInfo {
                        id: id.to_string(),
                        compiled,
                    });
                }
            }
        }
        
        Ok(result)
    }
    
    /// Проверить существование
    pub fn exists(&self, id: &str) -> bool {
        self.base_path.join(id).exists()
    }
    
    // ═══════════════════════════════════════════════════════════
    // КОМПИЛЯЦИЯ
    // ═══════════════════════════════════════════════════════════
    
    pub fn compile(&self, id: &str) -> Result<CompilationResult> {
        let dir = self.base_path.join(id);
        if !dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        tracing::info!("📦 Compiling '{}'...", id);
        
        let output = Command::new("cargo")
            .args(["build", "--release", "--manifest-path"])
            .arg(dir.join("Cargo.toml"))
            .output()
            .context("Failed to run cargo")?;
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}\n{}", stdout, stderr);
        
        if output.status.success() {
            let lib_path = self.lib_path_for(&dir, id);
            if lib_path.exists() {
                tracing::info!("✅ Compiled: {:?}", lib_path);
                Ok(CompilationResult {
                    success: true,
                    lib_path: Some(lib_path),
                    output: combined,
                    errors: vec![],
                })
            } else {
                anyhow::bail!("Library not found after compilation: {:?}", lib_path);
            }
        } else {
            let errors = self.parse_errors(&stderr);
            tracing::error!("❌ Compilation failed for '{}'", id);
            Ok(CompilationResult {
                success: false,
                lib_path: None,
                output: combined,
                errors,
            })
        }
    }
    
    pub fn check(&self, id: &str) -> Result<CompilationResult> {
        let dir = self.base_path.join(id);
        if !dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        let output = Command::new("cargo")
            .args(["check", "--manifest-path"])
            .arg(dir.join("Cargo.toml"))
            .output()?;
        
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), stderr);
        
        Ok(CompilationResult {
            success: output.status.success(),
            lib_path: None,
            output: combined,
            errors: if output.status.success() { vec![] } else { self.parse_errors(&stderr) },
        })
    }
    
    pub fn get_lib_path(&self, id: &str) -> Result<PathBuf> {
        let dir = self.base_path.join(id);
        let lib_path = self.lib_path_for(&dir, id);
        
        if lib_path.exists() {
            Ok(lib_path)
        } else {
            anyhow::bail!("Not compiled. Run compile first.")
        }
    }
    
    // ═══════════════════════════════════════════════════════════
    // HELPERS
    // ═══════════════════════════════════════════════════════════
    
    fn copy_cargo_toml(&self, dir: &Path, id: &str) -> Result<()> {
        let content = fs::read_to_string(self.templates_path.join("Cargo.toml"))?
            .replace("{{STRATEGY_NAME}}", id);
        fs::write(dir.join("Cargo.toml"), content)?;
        Ok(())
    }
    
    fn copy_types(&self, dir: &Path) -> Result<()> {
        fs::copy(
            self.templates_path.join("types.rs"),
            dir.join("src/types.rs"),
        )?;
        Ok(())
    }
    
    fn save_code(&self, dir: &Path, code: &str) -> Result<()> {
        let full = format!("mod types;\nuse types::*;\n\n{}", code);
        fs::write(dir.join("src/lib.rs"), full)?;
        Ok(())
    }
    
    fn load_code(&self, dir: &Path) -> Result<String> {
        let content = fs::read_to_string(dir.join("src/lib.rs"))?;
        Ok(content
            .strip_prefix("mod types;\nuse types::*;\n\n")
            .unwrap_or(&content)
            .to_string())
    }
    
    fn lib_path_for(&self, dir: &Path, id: &str) -> PathBuf {
        let name = if cfg!(target_os = "windows") {
            format!("{}.dll", id)
        } else if cfg!(target_os = "macos") {
            format!("lib{}.dylib", id)
        } else {
            format!("lib{}.so", id)
        };
        dir.join("target/release").join(name)
    }
    
    fn parse_errors(&self, stderr: &str) -> Vec<String> {
        stderr.lines()
            .filter(|l| l.contains("error:") || l.contains("error[E"))
            .map(String::from)
            .collect()
    }
}