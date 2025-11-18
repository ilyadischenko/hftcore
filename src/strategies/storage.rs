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
    pub id: String,           // "ma_strategy"
    pub name: String,         // "Moving Average Strategy"
    pub symbol: String,       // "BTCUSDT"
    pub enabled: bool,        // включена ли
    pub open_positions: bool,
    pub created_at: i64,      // timestamp
    pub updated_at: i64,      // timestamp
}

/// Полная информация о стратегии
#[derive(Debug, Clone)]
pub struct Strategy {
    pub metadata: StrategyMetadata,
    pub code: String,         // Rust код
}

/// Результат компиляции
#[derive(Debug)]
pub struct CompilationResult {
    pub success: bool,
    pub lib_path: Option<PathBuf>,  // путь к .so/.dll/.dylib
    pub output: String,              // вывод cargo
    pub errors: Vec<String>,         // ошибки компиляции
}

// ═══════════════════════════════════════════════════════════
// STORAGE
// ═══════════════════════════════════════════════════════════

pub struct StrategyStorage {
    base_path: PathBuf,  // "strategies/"
}

impl StrategyStorage {
    /// Создать хранилище
    /// 
    /// # Аргументы
    /// * `base_path` - путь к папке со стратегиями (обычно "strategies/")
    pub fn new(base_path: &str) -> Result<Self> {
        let path = PathBuf::from(base_path);
        
        // Создаём базовую папку если её нет
        if !path.exists() {
            fs::create_dir_all(&path)
                .context("Failed to create strategies directory")?;
        }
        
        Ok(Self { base_path: path })
    }
    
    // ═══════════════════════════════════════════════════════════
    // СОЗДАНИЕ стратегии
    // ═══════════════════════════════════════════════════════════
    
    /// Создать новую стратегию
    /// 
    /// Создаёт структуру:
    /// ```
    /// strategies/
    /// └── strategy_id/
    ///     ├── Cargo.toml
    ///     ├── metadata.json
    ///     └── src/
    ///         └── lib.rs
    /// ```
    pub fn create(&self, strategy: Strategy) -> Result<()> {
        let strategy_dir = self.get_strategy_dir(&strategy.metadata.id);
        
        // Проверяем что не существует
        if strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' already exists", strategy.metadata.id);
        }
        
        // Создаём структуру папок
        let src_dir = strategy_dir.join("src");
        fs::create_dir_all(&src_dir)
            .context("Failed to create strategy directories")?;
        
        // 1. Создаём Cargo.toml
        self.create_cargo_toml(&strategy_dir, &strategy.metadata.id)?;
        
        // 2. Сохраняем код в src/lib.rs
        self.save_code(&strategy_dir, &strategy.code)?;
        
        // 3. Сохраняем метаданные в metadata.json
        self.save_metadata(&strategy_dir, &strategy.metadata)?;
        
        tracing::info!("✅ Strategy '{}' created", strategy.metadata.id);
        Ok(())
    }
    
    // ═══════════════════════════════════════════════════════════
    // ЗАГРУЗКА стратегии
    // ═══════════════════════════════════════════════════════════
    
    /// Загрузить стратегию по ID
    pub fn load(&self, id: &str) -> Result<Strategy> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        // Загружаем метаданные
        let metadata = self.load_metadata(&strategy_dir)?;
        
        // Загружаем код
        let code = self.load_code(&strategy_dir)?;
        
        Ok(Strategy { metadata, code })
    }
    
    // ═══════════════════════════════════════════════════════════
    // ОБНОВЛЕНИЕ стратегии
    // ═══════════════════════════════════════════════════════════
    
    /// Обновить код стратегии
    pub fn update_code(&self, id: &str, new_code: String) -> Result<()> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        // Сохраняем новый код
        self.save_code(&strategy_dir, &new_code)?;
        
        // Обновляем timestamp
        let mut metadata = self.load_metadata(&strategy_dir)?;
        metadata.updated_at = chrono::Utc::now().timestamp();
        self.save_metadata(&strategy_dir, &metadata)?;
        
        tracing::info!("✏️ Code updated for '{}'", id);
        Ok(())
    }
    
    /// Обновить метаданные (название, символ и т.д.)
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
    
    /// Удалить стратегию полностью (включая скомпилированные файлы)
    pub fn delete(&self, id: &str) -> Result<()> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        // Удаляем всю папку
        fs::remove_dir_all(&strategy_dir)
            .context("Failed to delete strategy directory")?;
        
        tracing::info!("🗑️ Strategy '{}' deleted", id);
        Ok(())
    }
    
    // ═══════════════════════════════════════════════════════════
    // СПИСОК стратегий
    // ═══════════════════════════════════════════════════════════
    
    /// Получить список всех стратегий (только метаданные)
    pub fn list(&self) -> Result<Vec<StrategyMetadata>> {
        let mut strategies = Vec::new();
        
        // Читаем все папки в strategies/
        for entry in fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();
            
            // Только директории
            if !path.is_dir() {
                continue;
            }
            
            // Пытаемся загрузить метаданные
            if let Ok(metadata) = self.load_metadata(&path) {
                strategies.push(metadata);
            }
        }
        
        Ok(strategies)
    }
    
    // ═══════════════════════════════════════════════════════════
    // КОМПИЛЯЦИЯ
    // ═══════════════════════════════════════════════════════════
    
    /// Скомпилировать стратегию в динамическую библиотеку
    pub fn compile(&self, id: &str) -> Result<CompilationResult> {
        let strategy_dir = self.get_strategy_dir(id);
        
        if !strategy_dir.exists() {
            anyhow::bail!("Strategy '{}' not found", id);
        }
        
        let manifest_path = strategy_dir.join("Cargo.toml");
        
        tracing::info!("📦 Compiling '{}'...", id);
        
        // Запускаем cargo build
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
            // Определяем имя библиотеки в зависимости от ОС
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
            // Парсим ошибки компиляции
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
    
    /// Проверить синтаксис без полной компиляции (быстро)
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
    
    /// Получить путь к папке стратегии
    fn get_strategy_dir(&self, id: &str) -> PathBuf {
        self.base_path.join(id)
    }
    
    /// Создать Cargo.toml для стратегии
    fn create_cargo_toml(&self, strategy_dir: &Path, id: &str) -> Result<()> {
        let cargo_toml = format!(
r#"[package]
name = "{}"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
strategy_api = {{ path = "../../strategy_api" }}

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
"#,
            id
        );
        
        let cargo_path = strategy_dir.join("Cargo.toml");
        fs::write(&cargo_path, cargo_toml)
            .context("Failed to write Cargo.toml")?;
        
        Ok(())
    }
    
    /// Сохранить код в src/lib.rs
    fn save_code(&self, strategy_dir: &Path, code: &str) -> Result<()> {
        let lib_path = strategy_dir.join("src").join("lib.rs");
        fs::write(&lib_path, code)
            .context("Failed to write lib.rs")?;
        Ok(())
    }
    
    /// Загрузить код из src/lib.rs
    fn load_code(&self, strategy_dir: &Path) -> Result<String> {
        let lib_path = strategy_dir.join("src").join("lib.rs");
        fs::read_to_string(&lib_path)
            .context("Failed to read lib.rs")
    }
    
    /// Сохранить метаданные в metadata.json
    fn save_metadata(&self, strategy_dir: &Path, metadata: &StrategyMetadata) -> Result<()> {
        let metadata_path = strategy_dir.join("metadata.json");
        let json = serde_json::to_string_pretty(metadata)
            .context("Failed to serialize metadata")?;
        fs::write(&metadata_path, json)
            .context("Failed to write metadata.json")?;
        Ok(())
    }
    
    /// Загрузить метаданные из metadata.json
    fn load_metadata(&self, strategy_dir: &Path) -> Result<StrategyMetadata> {
        let metadata_path = strategy_dir.join("metadata.json");
        let json = fs::read_to_string(&metadata_path)
            .context("Failed to read metadata.json")?;
        let metadata = serde_json::from_str(&json)
            .context("Failed to parse metadata.json")?;
        Ok(metadata)
    }
    
    /// Получить имя библиотеки в зависимости от ОС
    fn get_lib_name(&self, id: &str) -> String {
        #[cfg(target_os = "linux")]
        return format!("lib{}.so", id);
        
        #[cfg(target_os = "macos")]
        return format!("lib{}.dylib", id);
        
        #[cfg(target_os = "windows")]
        return format!("{}.dll", id);
    }
    
    /// Парсить ошибки компиляции из stderr
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

/// Создать стратегию из кода и метаданных
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