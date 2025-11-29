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
use crate::user_data::{UserDataManager, UserDataEvent};

// ═══════════════════════════════════════════════════════════
// FFI TYPES
// ═══════════════════════════════════════════════════════════

pub type UserDataReceiverHolder = std::sync::Mutex<Option<broadcast::Receiver<UserDataEvent>>>;

#[repr(C)]
pub struct StrategyConfig {
    pub symbol: [u8; 32],
    pub symbol_len: u8,
    pub params_json: *const std::os::raw::c_char,
    pub stop_flag: *const AtomicBool,
    pub user_data_rx: *const UserDataReceiverHolder,
}

type RunFn = unsafe extern "C" fn(
    rx: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32;

// ═══════════════════════════════════════════════════════════
// INSTANCE INFO
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct InstanceInfo {
    pub instance_id: String,
    pub strategy_id: String,
    pub symbol: String,
    pub params: serde_json::Value,
    pub started_at: i64,
    pub has_user_data: bool,
}

// ═══════════════════════════════════════════════════════════
// RUNNING INSTANCE
// ═══════════════════════════════════════════════════════════

struct RunningInstance {
    info: InstanceInfo,
    _lib: Arc<Library>,
    stop_flag: Arc<AtomicBool>,
    task: JoinHandle<i32>,
    bridge_task: JoinHandle<()>,
    _user_data_holder: Option<Arc<UserDataReceiverHolder>>,
}

// ═══════════════════════════════════════════════════════════
// STRATEGY RUNNER
// ═══════════════════════════════════════════════════════════

pub struct StrategyRunner {
    instances: Arc<DashMap<String, RunningInstance>>,
    user_data_manager: Arc<UserDataManager>,
}

impl StrategyRunner {
    pub fn new(user_data_manager: Arc<UserDataManager>) -> Arc<Self> { 
        let runner = Arc::new(Self {
            instances: Arc::new(DashMap::new()),
            user_data_manager,
        });
        
        let instances = runner.instances.clone();
        tokio::spawn(async move {
            tracing::info!("🧹 Cleanup loop started");
            Self::cleanup_loop(instances).await;
        });
        
        runner
    }
    
    async fn cleanup_loop(instances: Arc<DashMap<String, RunningInstance>>) {
        let mut check_count = 0u64;
        
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            check_count += 1;
            
            if check_count % 10 == 0 {
                let count = instances.len();
                if count > 0 {
                    tracing::debug!("🧹 Cleanup check #{}: {} instances", check_count, count);
                }
            }
            
            let mut finished = Vec::new();
            
            for entry in instances.iter() {
                let id = entry.key();
                let inst = entry.value();
                
                if inst.task.is_finished() {
                    tracing::info!(
                        "🔍 Instance '{}': task=DONE, bridge={}", 
                        id, 
                        if inst.bridge_task.is_finished() { "DONE" } else { "running" }
                    );
                    finished.push(id.clone());
                }
            }
            
            for id in finished {
                if let Some((_, inst)) = instances.remove(&id) {
                    if !inst.bridge_task.is_finished() {
                        inst.stop_flag.store(true, Ordering::Relaxed);
                        inst.bridge_task.abort();
                    }
                    
                    let code = match inst.task.await {
                        Ok(c) => Some(c),
                        Err(e) => {
                            tracing::error!("❌ Task '{}' panicked: {:?}", id, e);
                            None
                        }
                    };
                    
                    tracing::info!("🧹 Cleaned '{}' (exit: {:?})", id, code);
                }
            }
        }
    }
    
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
        
        // ═══════════════════════════════════════════════════════════
        // User Data Stream
        // ═══════════════════════════════════════════════════════════
        let user_data_rx: Option<broadcast::Receiver<UserDataEvent>> = params
            .get("api_key")
            .and_then(|v| v.as_str())
            .and_then(|api_key| {
                let rx = self.user_data_manager.subscribe(api_key);
                if rx.is_some() {
                    tracing::info!("📡 User Data Stream attached for '{}'", instance_id);
                }
                rx
            });
        
        let has_user_data = user_data_rx.is_some();
        
        let user_data_holder: Option<Arc<UserDataReceiverHolder>> = user_data_rx
            .map(|rx| Arc::new(std::sync::Mutex::new(Some(rx))));
        
        // Загружаем библиотеку
        let lib: Arc<Library> = Arc::new(unsafe { Library::new(&lib_path)? });
        let run_fn: RunFn = unsafe { *lib.get(b"run")? };
        
        let (sync_tx, sync_rx) = bounded::<CEvent>(8192);
        let stop_flag = Arc::new(AtomicBool::new(false));
        
        // Bridge task
        let bridge_task = {
            let instance_id = instance_id.clone();
            let stop_flag = stop_flag.clone();
            
            tokio::spawn(async move {
                Self::bridge_loop(instance_id, event_rx, sync_tx, stop_flag).await;
            })
        };
        
        // Strategy task
        // ═══════════════════════════════════════════════════════════
        // ВАЖНО: передаём Arc внутрь, указатель получаем там
        // ═══════════════════════════════════════════════════════════
        let task = {
            let instance_id = instance_id.clone();
            let symbol = symbol.clone();
            let lib = lib.clone();
            let stop_flag = stop_flag.clone();
            let user_data_holder = user_data_holder.clone();  // Clone Arc (или None)
            
            tokio::task::spawn_blocking(move || {
                // Получаем указатель ВНУТРИ spawn_blocking
                let user_data_ptr: *const UserDataReceiverHolder = user_data_holder
                    .as_ref()
                    .map(|h| Arc::as_ptr(h))
                    .unwrap_or(std::ptr::null());
                
                let result = Self::run_strategy(
                    instance_id.clone(), 
                    lib, 
                    run_fn, 
                    sync_rx, 
                    symbol, 
                    params_json,
                    stop_flag,
                    user_data_ptr,
                    user_data_holder,  // Передаём для ownership
                );
                
                tracing::info!("📤 Task '{}' returning {}", instance_id, result);
                result
            })
        };
        
        let info = InstanceInfo {
            instance_id: instance_id.clone(),
            strategy_id,
            symbol,
            params,
            started_at: chrono::Utc::now().timestamp(),
            has_user_data,
        };
        
        self.instances.insert(instance_id.clone(), RunningInstance {
            info: info.clone(),
            _lib: lib,
            stop_flag,
            task,
            bridge_task,
            _user_data_holder: user_data_holder,
        });
        
        tracing::info!("✅ Instance '{}' started (user_data={})", instance_id, has_user_data);
        Ok(info)
    }
    
    async fn bridge_loop(
        instance_id: String,
        mut event_rx: broadcast::Receiver<CEvent>,
        sync_tx: Sender<CEvent>,
        stop_flag: Arc<AtomicBool>,
    ) {
        tracing::debug!("🌉 Bridge '{}' started", instance_id);
        let mut dropped = 0u64;
        
        loop {
            if stop_flag.load(Ordering::Relaxed) {
                tracing::debug!("🌉 Bridge '{}' stopping (flag)", instance_id);
                break;
            }
            
            match tokio::time::timeout(
                tokio::time::Duration::from_millis(100),
                event_rx.recv()
            ).await {
                Ok(Ok(event)) => {
                    if sync_tx.try_send(event).is_err() {
                        dropped += 1;
                        if dropped % 1000 == 0 {
                            tracing::warn!("⚠️ '{}' lagging: {} dropped", instance_id, dropped);
                        }
                    }
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    tracing::debug!("🌉 Bridge '{}' stopping (closed)", instance_id);
                    break;
                }
                Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                    tracing::warn!("'{}' lagged {} msgs", instance_id, n);
                }
                Err(_) => {}
            }
        }
        
        tracing::debug!("🌉 Bridge '{}' stopped", instance_id);
    }
    
    fn run_strategy(
        instance_id: String,
        lib: Arc<Library>,
        run_fn: RunFn,
        sync_rx: Receiver<CEvent>,
        symbol: String,
        params_json: String,
        stop_flag: Arc<AtomicBool>,
        user_data_ptr: *const UserDataReceiverHolder,
        _user_data_holder: Option<Arc<UserDataReceiverHolder>>,  // Держим ownership
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
            stop_flag: Arc::as_ptr(&stop_flag),
            user_data_rx: user_data_ptr,
        };
        
        let rx_ptr = Box::into_raw(Box::new(sync_rx));
        
        let result = unsafe { run_fn(rx_ptr, place_order, cancel_order, config) };
        
        stop_flag.store(true, Ordering::Relaxed);
        
        unsafe { let _ = Box::from_raw(rx_ptr); }
        drop(lib);
        
        tracing::info!("🏁 Strategy thread '{}' finished (code={})", instance_id, result);
        result
    }
    
    pub async fn stop(&self, instance_id: &str) -> Result<()> {
        let entry = self.instances.get(instance_id)
            .ok_or_else(|| anyhow::anyhow!("Instance '{}' not found", instance_id))?;
        
        tracing::info!("🛑 Stopping '{}'...", instance_id);
        
        entry.stop_flag.store(true, Ordering::Relaxed);
        
        drop(entry);
        
        for i in 0..100 {
            if !self.instances.contains_key(instance_id) {
                tracing::info!("✅ '{}' stopped after {}ms", instance_id, i * 100);
                return Ok(());
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
        
        if let Some((_, inst)) = self.instances.remove(instance_id) {
            inst.bridge_task.abort();
            tracing::warn!("⚠️ '{}' force removed", instance_id);
        }
        
        Ok(())
    }
    
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
    
    pub fn list(&self) -> Vec<InstanceInfo> {
        self.instances.iter().map(|e| e.value().info.clone()).collect()
    }
    
    pub fn list_for(&self, strategy_id: &str) -> Vec<InstanceInfo> {
        self.instances.iter()
            .filter(|e| e.value().info.strategy_id == strategy_id)
            .map(|e| e.value().info.clone())
            .collect()
    }
    
    pub fn get(&self, instance_id: &str) -> Option<InstanceInfo> {
        self.instances.get(instance_id).map(|e| e.value().info.clone())
    }
    
    pub fn is_running(&self, instance_id: &str) -> bool {
        self.instances.contains_key(instance_id)
    }
}