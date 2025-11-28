// src/strategies/manager.rs

use libloading::Library;
use tokio::sync::broadcast;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;
use dashmap::DashMap;
use anyhow::Result;
use crossbeam::channel::{bounded, Receiver, Sender};
use std::ffi::CString;
use serde::Serialize;

use crate::ffi_types::CEvent;
use crate::strategies::order::{PlaceOrderFn, CancelOrderFn, place_order, cancel_order};

// ═══════════════════════════════════════════════════════════
// FFI
// ═══════════════════════════════════════════════════════════

#[repr(C)]
pub struct StrategyConfig {
    pub symbol: [u8; 32],
    pub symbol_len: u8,
    pub params_json: *const std::os::raw::c_char,
}

type RunFn = unsafe extern "C" fn(
    rx: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32;

type StopFn = unsafe extern "C" fn();

// ═══════════════════════════════════════════════════════════
// INSTANCE INFO (публичный)
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct InstanceInfo {
    pub instance_id: String,
    pub strategy_id: String,
    pub symbol: String,
    pub params: serde_json::Value,
    pub started_at: i64,
}

// ═══════════════════════════════════════════════════════════
// INTERNAL
// ═══════════════════════════════════════════════════════════

struct RunningInstance {
    info: InstanceInfo,
    _lib: Arc<Library>,
    stop_fn: StopFn,
    task: JoinHandle<i32>,
    bridge_task: JoinHandle<()>,
    shutdown: Arc<AtomicBool>,
}

// ═══════════════════════════════════════════════════════════
// RUNNER
// ═══════════════════════════════════════════════════════════

pub struct StrategyRunner {
    instances: Arc<DashMap<String, RunningInstance>>,
}

impl StrategyRunner {
    pub fn new() -> Arc<Self> {
        let runner = Arc::new(Self {
            instances: Arc::new(DashMap::new()),
        });
        
        // Фоновая очистка завершённых
        let instances = runner.instances.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                
                let finished: Vec<_> = instances.iter()
                    .filter(|e| e.value().task.is_finished())
                    .map(|e| e.key().clone())
                    .collect();
                
                for id in finished {
                    if let Some((_, inst)) = instances.remove(&id) {
                        let code = inst.task.await.ok();
                        inst.bridge_task.abort();
                        tracing::info!("🧹 Cleaned '{}' (exit: {:?})", id, code);
                    }
                }
            }
        });
        
        runner
    }
    
    /// Запустить инстанс: strategy_id + symbol → instance_id
    pub async fn start(
        &self,
        strategy_id: String,
        symbol: String,
        lib_path: PathBuf,
        event_rx: broadcast::Receiver<CEvent>,
        params: serde_json::Value,
    ) -> Result<InstanceInfo> {
        let instance_id = format!("{}:{}", strategy_id, symbol.to_uppercase());
        
        if self.instances.contains_key(&instance_id) {
            anyhow::bail!("Instance '{}' already running", instance_id);
        }
        
        let params_json = serde_json::to_string(&params)?;
        
        tracing::info!("📦 Starting '{}' with params: {}", instance_id, params_json);
        
        // Загружаем библиотеку
        let lib: Arc<Library> = Arc::new(unsafe { Library::new(&lib_path)? });
        let run_fn: RunFn = unsafe { *lib.get(b"run")? };
        let stop_fn: StopFn = unsafe { *lib.get(b"stop")? };
        
        // Каналы
        let (sync_tx, sync_rx) = bounded::<CEvent>(8192);
        let shutdown = Arc::new(AtomicBool::new(false));
        
        // Bridge task
        let bridge_task = {
            let instance_id = instance_id.clone();
            let shutdown = shutdown.clone();
            
            tokio::spawn(async move {
                Self::bridge_loop(instance_id, event_rx, sync_tx, shutdown).await;
            })
        };
        
        // Strategy task
        let task = {
            let instance_id = instance_id.clone();
            let symbol = symbol.clone();
            let lib = lib.clone();
            
            tokio::task::spawn_blocking(move || {
                Self::run_strategy(instance_id, lib, run_fn, sync_rx, symbol, params_json)
            })
        };
        
        let info = InstanceInfo {
            instance_id: instance_id.clone(),
            strategy_id,
            symbol,
            params,
            started_at: chrono::Utc::now().timestamp(),
        };
        
        self.instances.insert(instance_id.clone(), RunningInstance {
            info: info.clone(),
            _lib: lib,
            stop_fn,
            task,
            bridge_task,
            shutdown,
        });
        
        tracing::info!("✅ Instance '{}' started", instance_id);
        Ok(info)
    }
    
    async fn bridge_loop(
        instance_id: String,
        mut event_rx: broadcast::Receiver<CEvent>,
        sync_tx: Sender<CEvent>,
        shutdown: Arc<AtomicBool>,
    ) {
        let mut dropped = 0u64;
        
        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            
            match event_rx.recv().await {
                Ok(event) => {
                    if sync_tx.try_send(event).is_err() {
                        dropped += 1;
                        if dropped % 1000 == 0 {
                            tracing::warn!("⚠️ '{}' lagging: {} dropped", instance_id, dropped);
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("'{}' lagged {} msgs", instance_id, n);
                }
            }
        }
    }
    
    fn run_strategy(
        instance_id: String,
        lib: Arc<Library>,
        run_fn: RunFn,
        sync_rx: Receiver<CEvent>,
        symbol: String,
        params_json: String,
    ) -> i32 {
        tracing::info!("🚀 Strategy thread '{}' started", instance_id);
        
        let params_cstring = CString::new(params_json).expect("Invalid JSON");
        
        let mut symbol_bytes = [0u8; 32];
        let bytes = symbol.as_bytes();
        let len = bytes.len().min(31);
        symbol_bytes[..len].copy_from_slice(&bytes[..len]);
        
        let config = StrategyConfig {
            symbol: symbol_bytes,
            symbol_len: len as u8,
            params_json: params_cstring.as_ptr(),
        };
        
        let rx_ptr = Box::into_raw(Box::new(sync_rx));
        
        let result = unsafe { run_fn(rx_ptr, place_order, cancel_order, config) };
        
        unsafe { let _ = Box::from_raw(rx_ptr); }
        drop(lib);
        
        tracing::info!("🏁 Strategy thread '{}' finished (code={})", instance_id, result);
        result
    }
    
    /// Остановить конкретный инстанс
    pub async fn stop(&self, instance_id: &str) -> Result<()> {
        let entry = self.instances.get(instance_id)
            .ok_or_else(|| anyhow::anyhow!("Instance '{}' not found", instance_id))?;
        
        tracing::info!("🛑 Stopping '{}'...", instance_id);
        
        entry.shutdown.store(true, Ordering::Relaxed);
        unsafe { (entry.stop_fn)(); }
        
        drop(entry);
        
        // Ждём очистки
        for _ in 0..100 {
            if !self.instances.contains_key(instance_id) {
                tracing::info!("✅ '{}' stopped", instance_id);
                return Ok(());
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
        
        // Force remove
        self.instances.remove(instance_id);
        tracing::warn!("⚠️ '{}' force removed", instance_id);
        Ok(())
    }
    
    /// Остановить все инстансы стратегии
    pub async fn stop_all(&self, strategy_id: &str) -> Vec<String> {
        let to_stop: Vec<_> = self.instances.iter()
            .filter(|e| e.value().info.strategy_id == strategy_id)
            .map(|e| e.key().clone())
            .collect();
        
        for id in &to_stop {
            let _ = self.stop(id).await;
        }
        
        to_stop
    }
    
    /// Список всех запущенных инстансов
    pub fn list(&self) -> Vec<InstanceInfo> {
        self.instances.iter()
            .map(|e| e.value().info.clone())
            .collect()
    }
    
    /// Инстансы конкретной стратегии
    pub fn list_for(&self, strategy_id: &str) -> Vec<InstanceInfo> {
        self.instances.iter()
            .filter(|e| e.value().info.strategy_id == strategy_id)
            .map(|e| e.value().info.clone())
            .collect()
    }
    
    /// Получить информацию об инстансе
    pub fn get(&self, instance_id: &str) -> Option<InstanceInfo> {
        self.instances.get(instance_id).map(|e| e.value().info.clone())
    }
    
    pub fn is_running(&self, instance_id: &str) -> bool {
        self.instances.contains_key(instance_id)
    }
}