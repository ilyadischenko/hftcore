// src/strategies/storage.rs

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use anyhow::{Result, Context};

// ═══════════════════════════════════════════════════════════
// ТИПЫ ДАННЫХ
// ═══════════════════════════════════════════════════════════

/// Метаданные стратегии (хранятся в JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyMetadata {
    pub id: String,
    pub name: String,
    pub symbol: String,
    pub enabled: bool,
    pub open_positions: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Полная информация о стратегии
#[derive(Debug, Clone)]
pub struct Strategy {
    pub metadata: StrategyMetadata,
    pub code: String,
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
    base_path: PathBuf,           // "strategies/db"
    templates_path: PathBuf,      // "copy_into_strategies"
}

impl StrategyStorage {
    /// Создать хранилище
    /// 
    /// # Аргументы
    /// * `base_path` - путь к папке со стратегиями (обычно "strategies/db")
    pub fn new(base_path: &str) -> Result<Self> {
        let base = PathBuf::from(base_path);
        
        // Создаём базовую папку если её нет
        if !base.exists() {
            fs::create_dir_all(&base)
                .context("Failed to create strategies directory")?;
        }
        
        // ═══════════════════════════════════════════════════════════
        // ПУТЬ К ШАБЛОНАМ
        // ═══════════════════════════════════════════════════════════
        let templates = PathBuf::from("copy_into_strategies");
        
        if !templates.exists() {
            anyhow::bail!(
                "Templates directory not found: {:?}\nPlease create 'copy_into_strategies/' with types.rs and Cargo.toml",
                templates
            );
        }
        
        // Проверяем наличие обязательных файлов
        let types_rs = templates.join("types.rs");
        let cargo_toml = templates.join("Cargo.toml");
        
        if !types_rs.exists() {
            anyhow::bail!("Missing file: {:?}", types_rs);
        }
        if !cargo_toml.exists() {
            anyhow::bail!("Missing file: {:?}", cargo_toml);
        }
        
        tracing::info!("✅ Strategy templates loaded from {:?}", templates);
        
        Ok(Self { 
            base_path: base,
            templates_path: templates,
        })
    }
    
    // ═══════════════════════════════════════════════════════════
    // СОЗДАНИЕ стратегии
    // ═══════════════════════════════════════════════════════════
    
    pub fn create(&self, strategy: Strategy) -> Result<()> {
        let strategy_dir = self.get_strategy_dir(&strategy.metadata.id);
        
        if strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' already exists", strategy.metadata.id);
        }
        
        let src_dir = strategy_dir.join("src");
        fs::create_dir_all(&src_dir)
            .context("Failed to create strategy directories")?;
        
        // ═══════════════════════════════════════════════════════════
        // 1. КОПИРУЕМ Cargo.toml
        // ═══════════════════════════════════════════════════════════
        self.copy_cargo_toml(&strategy_dir, &strategy.metadata.id)?;
        
        // ═══════════════════════════════════════════════════════════
        // 2. КОПИРУЕМ types.rs
        // ═══════════════════════════════════════════════════════════
        self.copy_types(&strategy_dir)?;
        
        // ═══════════════════════════════════════════════════════════
        // 3. Сохраняем код в src/lib.rs (с импортом types)
        // ═══════════════════════════════════════════════════════════
        self.save_code(&strategy_dir, &strategy.code)?;
        
        // ═══════════════════════════════════════════════════════════
        // 4. Сохраняем метаданные
        // ═══════════════════════════════════════════════════════════
        self.save_metadata(&strategy_dir, &strategy.metadata)?;
        
        tracing::info!("✅ Strategy '{}' created", strategy.metadata.id);
        Ok(())
    }
    
    // ═══════════════════════════════════════════════════════════
    // ЗАГРУЗКА стратегии
    // ═══════════════════════════════════════════════════════════
    
    pub fn load(&self, id: &str) -> Result<Strategy> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        let metadata = self.load_metadata(&strategy_dir)?;
        let code = self.load_code(&strategy_dir)?;
        
        Ok(Strategy { metadata, code })
    }
    
    // ═══════════════════════════════════════════════════════════
    // ОБНОВЛЕНИЕ стратегии
    // ═══════════════════════════════════════════════════════════
    
    pub fn update_code(&self, id: &str, new_code: String) -> Result<()> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        // Проверяем наличие types.rs, если нет - копируем
        let types_path = strategy_dir.join("src").join("types.rs");
        if !types_path.exists() {
            tracing::warn!("types.rs not found for '{}', copying from template", id);
            self.copy_types(&strategy_dir)?;
        }
        
        self.save_code(&strategy_dir, &new_code)?;
        
        let mut metadata = self.load_metadata(&strategy_dir)?;
        metadata.updated_at = chrono::Utc::now().timestamp();
        self.save_metadata(&strategy_dir, &metadata)?;
        
        tracing::info!("✏️ Code updated for '{}'", id);
        Ok(())
    }
    
    pub fn update_metadata(
        &self,
        id: &str,
        name: Option<String>,
        symbol: Option<String>,
        enabled: Option<bool>,
        open_positions: Option<bool>,
    ) -> Result<()> {
        let strategy_dir = self.get_strategy_dir(id);
        let mut metadata = self.load_metadata(&strategy_dir)?;
        
        if let Some(n) = name {
            metadata.name = n;
        }
        if let Some(s) = symbol {
            metadata.symbol = s;
        }
        if let Some(e) = enabled {
            metadata.enabled = e;
        }
        if let Some(op) = open_positions {
            metadata.open_positions = op;
        }
        
        metadata.updated_at = chrono::Utc::now().timestamp();
        self.save_metadata(&strategy_dir, &metadata)?;
        
        tracing::info!("⚙️ Metadata updated for '{}'", id);
        Ok(())
    }
    
    // ═══════════════════════════════════════════════════════════
    // УДАЛЕНИЕ стратегии
    // ═══════════════════════════════════════════════════════════
    
    pub fn delete(&self, id: &str) -> Result<()> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        fs::remove_dir_all(&strategy_dir)
            .context("Failed to delete strategy directory")?;
        
        tracing::info!("🗑️ Strategy '{}' deleted", id);
        Ok(())
    }
    
    // ═══════════════════════════════════════════════════════════
    // СПИСОК стратегий
    // ═══════════════════════════════════════════════════════════
    
    pub fn list(&self) -> Result<Vec<StrategyMetadata>> {
        let mut strategies = Vec::new();
        
        for entry in fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();
            
            if !path.is_dir() {
                continue;
            }
            
            if let Ok(metadata) = self.load_metadata(&path) {
                strategies.push(metadata);
            }
        }
        
        Ok(strategies)
    }
    
    // ═══════════════════════════════════════════════════════════
    // КОМПИЛЯЦИЯ
    // ═══════════════════════════════════════════════════════════
    
    pub fn compile(&self, id: &str) -> Result<CompilationResult> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        let manifest_path = strategy_dir.join("Cargo.toml");
        
        tracing::info!("📦 Compiling '{}'...", id);
        
        let output = Command::new("cargo")
            .args(&[
                "build",
                "--release",
                "--manifest-path",
                manifest_path.to_str().unwrap(),
            ])
            .output()
            .context("Failed to run cargo build")?;
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined_output = format!("{}\n{}", stdout, stderr);
        
        if output.status.success() {
            let lib_name = self.get_lib_name(id);
            let lib_path = strategy_dir
                .join("target")
                .join("release")
                .join(&lib_name);
            
            if lib_path.exists() {
                tracing::info!("✅ Compiled: {:?}", lib_path);
                Ok(CompilationResult {
                    success: true,
                    lib_path: Some(lib_path),
                    output: combined_output,
                    errors: Vec::new(),
                })
            } else {
                anyhow::bail!("Library file not found after compilation: {:?}", lib_path);
            }
        } else {
            let errors = self.parse_compilation_errors(&stderr);
            tracing::error!("❌ Compilation failed for '{}'", id);
            Ok(CompilationResult {
                success: false,
                lib_path: None,
                output: combined_output,
                errors,
            })
        }
    }
    
    pub fn check(&self, id: &str) -> Result<CompilationResult> {
        let strategy_dir = self.get_strategy_dir(id);
        let manifest_path = strategy_dir.join("Cargo.toml");
        
        tracing::info!("🔍 Checking '{}'...", id);
        
        let output = Command::new("cargo")
            .args(&[
                "check",
                "--manifest-path",
                manifest_path.to_str().unwrap(),
            ])
            .output()
            .context("Failed to run cargo check")?;
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined_output = format!("{}\n{}", stdout, stderr);
        
        if output.status.success() {
            tracing::info!("✅ Check passed for '{}'", id);
            Ok(CompilationResult {
                success: true,
                lib_path: None,
                output: combined_output,
                errors: Vec::new(),
            })
        } else {
            let errors = self.parse_compilation_errors(&stderr);
            Ok(CompilationResult {
                success: false,
                lib_path: None,
                output: combined_output,
                errors,
            })
        }
    }
    
    // ═══════════════════════════════════════════════════════════
    // ВСПОМОГАТЕЛЬНЫЕ МЕТОДЫ
    // ═══════════════════════════════════════════════════════════
    
    fn get_strategy_dir(&self, id: &str) -> PathBuf {
        self.base_path.join(id)
    }
    
    // ═══════════════════════════════════════════════════════════
    // КОПИРОВАНИЕ Cargo.toml из шаблона
    // ═══════════════════════════════════════════════════════════
    fn copy_cargo_toml(&self, strategy_dir: &Path, id: &str) -> Result<()> {
        let template = self.templates_path.join("Cargo.toml");
        let dest = strategy_dir.join("Cargo.toml");
        
        // Читаем шаблон
        let mut content = fs::read_to_string(&template)
            .context("Failed to read Cargo.toml template")?;
        
        // Заменяем имя пакета
        content = content.replace("{{STRATEGY_NAME}}", id);
        
        // Сохраняем
        fs::write(&dest, content)
            .context("Failed to write Cargo.toml")?;
        
        tracing::debug!("Copied Cargo.toml for '{}'", id);
        Ok(())
    }
    
    // ═══════════════════════════════════════════════════════════
    // КОПИРОВАНИЕ types.rs из шаблона
    // ═══════════════════════════════════════════════════════════
    fn copy_types(&self, strategy_dir: &Path) -> Result<()> {
        let template = self.templates_path.join("types.rs");
        let dest = strategy_dir.join("src").join("types.rs");
        
        fs::copy(&template, &dest)
            .context("Failed to copy types.rs")?;
        
        tracing::debug!("Copied types.rs to {:?}", dest);
        Ok(())
    }
    
    // ═══════════════════════════════════════════════════════════
    // СОХРАНЕНИЕ кода с импортом types
    // ═══════════════════════════════════════════════════════════
    fn save_code(&self, strategy_dir: &Path, user_code: &str) -> Result<()> {
        let lib_path = strategy_dir.join("src").join("lib.rs");
        
        // Добавляем импорт types
        let full_code = format!("mod types;\nuse types::*;\n\n{}", user_code);
        
        fs::write(&lib_path, full_code)
            .context("Failed to write lib.rs")?;
        
        Ok(())
    }
    
    // ═══════════════════════════════════════════════════════════
    // ЗАГРУЗКА кода (убираем импорт)
    // ═══════════════════════════════════════════════════════════
    fn load_code(&self, strategy_dir: &Path) -> Result<String> {
        let lib_path = strategy_dir.join("src").join("lib.rs");
        let full_code = fs::read_to_string(&lib_path)
            .context("Failed to read lib.rs")?;
        
        // Убираем импорт types
        if full_code.starts_with("mod types;\nuse types::*;\n\n") {
            Ok(full_code
                .strip_prefix("mod types;\nuse types::*;\n\n")
                .unwrap()
                .to_string())
        } else {
            // Старый формат без импорта
            Ok(full_code)
        }
    }
    
    fn save_metadata(&self, strategy_dir: &Path, metadata: &StrategyMetadata) -> Result<()> {
        let metadata_path = strategy_dir.join("metadata.json");
        let json = serde_json::to_string_pretty(metadata)
            .context("Failed to serialize metadata")?;
        fs::write(&metadata_path, json)
            .context("Failed to write metadata.json")?;
        Ok(())
    }
    
    fn load_metadata(&self, strategy_dir: &Path) -> Result<StrategyMetadata> {
        let metadata_path = strategy_dir.join("metadata.json");
        let json = fs::read_to_string(&metadata_path)
            .context("Failed to read metadata.json")?;
        let metadata = serde_json::from_str(&json)
            .context("Failed to parse metadata.json")?;
        Ok(metadata)
    }

    pub fn get_lib_path(&self, id: &str) -> Result<PathBuf> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        let lib_name = self.get_lib_name(id);
        let lib_path = strategy_dir
            .join("target")
            .join("release")
            .join(&lib_name);
        
        if !lib_path.exists() {
            anyhow::bail!(
                "Library not compiled for strategy '{}'. Run compile('{}') first.\nExpected path: {:?}",
                id,
                id,
                lib_path
            );
        }
        
        Ok(lib_path)
    }
    
    fn get_lib_name(&self, id: &str) -> String {
        #[cfg(target_os = "linux")]
        return format!("lib{}.so", id);
        
        #[cfg(target_os = "macos")]
        return format!("lib{}.dylib", id);
        
        #[cfg(target_os = "windows")]
        return format!("{}.dll", id);
    }
    
    fn parse_compilation_errors(&self, stderr: &str) -> Vec<String> {
        stderr
            .lines()
            .filter(|line| {
                line.contains("error:") || 
                line.contains("error[E")
            })
            .map(|s| s.to_string())
            .collect()
    }
}

// ═══════════════════════════════════════════════════════════
// ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ
// ═══════════════════════════════════════════════════════════

impl Strategy {
    pub fn new(
        id: String,
        name: String,
        symbol: String,
        code: String,
    ) -> Self {
        let now = chrono::Utc::now().timestamp();
        
        Self {
            metadata: StrategyMetadata {
                id,
                name,
                symbol,
                enabled: false,
                open_positions: true,
                created_at: now,
                updated_at: now,
            },
            code,
        }
    }
}