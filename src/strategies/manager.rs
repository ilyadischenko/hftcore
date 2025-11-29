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

// Импортируем CEvent
use crate::ffi_types::CEvent;
use crate::strategies::order::{PlaceOrderFn, CancelOrderFn, place_order, cancel_order};
use crate::user_data::UserDataManager;

#[repr(C)]
pub struct StrategyConfig {
    pub symbol: [u8; 32],
    pub symbol_len: u8,
    pub params_json: *const std::os::raw::c_char,
    pub stop_flag: *const AtomicBool,
    // Указатель теперь просто void*, так как мы передаем Receiver<CEvent>
    // Стратегия сама скастует его обратно.
    pub user_data_rx: *const std::ffi::c_void,
}

type RunFn = unsafe extern "C" fn(
    rx: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32;

#[derive(Debug, Clone, Serialize)]
pub struct InstanceInfo {
    pub instance_id: String,
    pub strategy_id: String,
    pub symbol: String,
    pub params: serde_json::Value,
    pub started_at: i64,
    pub has_user_data: bool,
}

struct RunningInstance {
    info: InstanceInfo,
    _lib: Arc<Library>,
    stop_flag: Arc<AtomicBool>,
    task: JoinHandle<i32>,
    
    // Храним таски мостов, чтобы их можно было остановить
    market_bridge: JoinHandle<()>,
    user_bridge: Option<JoinHandle<()>>,
}

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
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            
            let mut finished = Vec::new();
            for entry in instances.iter() {
                if entry.value().task.is_finished() {
                    finished.push(entry.key().clone());
                }
            }
            
            for id in finished {
                if let Some((_, inst)) = instances.remove(&id) {
                    inst.stop_flag.store(true, Ordering::Relaxed);
                    inst.market_bridge.abort();
                    if let Some(ub) = inst.user_bridge {
                        ub.abort();
                    }
                    
                    let code = inst.task.await.unwrap_or(-1);
                    tracing::info!("🧹 Cleaned '{}' (exit: {})", id, code);
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
        tracing::info!("📦 Starting '{}'...", instance_id);
        
        // 1. Создаем ГЛАВНЫЙ канал (Common Channel)
        // В него будут писать ОБА источника (Market + User)
        let (sync_tx, sync_rx) = bounded::<CEvent>(8192);
        let stop_flag = Arc::new(AtomicBool::new(false));
        
        // 2. Bridge: Market Data -> Common Channel
        let market_bridge = {
            let mut rx = event_rx;
            let tx = sync_tx.clone();
            let stop = stop_flag.clone();
            let id = instance_id.clone();
            
            tokio::spawn(async move {
                loop {
                    if stop.load(Ordering::Relaxed) { break; }
                    match rx.recv().await {
                        Ok(event) => {
                            let _ = tx.try_send(event);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(_) => {} // Lagged
                    }
                }
                tracing::debug!("Market bridge stopped for {}", id);
            })
        };
        
        // 3. Bridge: User Data -> Common Channel (если есть api_key)
        let mut has_user_data = false;
        let user_bridge = if let Some(api_key) = params.get("api_key").and_then(|v| v.as_str()) {
            if let Some(mut user_rx) = self.user_data_manager.subscribe(api_key) {
                has_user_data = true;
                let tx = sync_tx.clone();
                let stop = stop_flag.clone();
                let id = instance_id.clone();
                
                Some(tokio::spawn(async move {
                    loop {
                        if stop.load(Ordering::Relaxed) { break; }
                        match user_rx.recv().await {
                            Ok(event) => {
                                // ПРОСТО ПРОКИДЫВАЕМ CEvent! Никакого JSON!
                                let _ = tx.try_send(event);
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                            Err(_) => {}
                        }
                    }
                    tracing::debug!("User bridge stopped for {}", id);
                }))
            } else {
                None
            }
        } else {
            None
        };
        
        // 4. Загружаем библиотеку и запускаем
        let lib = Arc::new(unsafe { Library::new(&lib_path)? });
        let run_fn: RunFn = unsafe { *lib.get(b"run")? };
        
        let task = {
            let instance_id = instance_id.clone();
            let symbol = symbol.clone();
            let lib = lib.clone(); // Держим ссылку на либу
            let stop_flag = stop_flag.clone();
            
            tokio::task::spawn_blocking(move || {
                let params_cstring = CString::new(params_json).unwrap();
                
                let mut symbol_bytes = [0u8; 32];
                let bytes = symbol.as_bytes();
                let len = bytes.len().min(31);
                symbol_bytes[..len].copy_from_slice(&bytes[..len]);
                
                // ВАЖНО: Мы передаем null в user_data_rx, потому что
                // теперь данные идут через ГЛАВНЫЙ канал (rx).
                // Стратегия должна читать ВСЁ из rx.
                let config = StrategyConfig {
                    symbol: symbol_bytes,
                    symbol_len: len as u8,
                    params_json: params_cstring.as_ptr(),
                    stop_flag: Arc::as_ptr(&stop_flag),
                    user_data_rx: std::ptr::null(), // <--- НЕ ИСПОЛЬЗУЕТСЯ (данные в rx)
                };
                
                let rx_ptr = Box::into_raw(Box::new(sync_rx));
                
                let res = unsafe { run_fn(rx_ptr, place_order, cancel_order, config) };
                
                unsafe { let _ = Box::from_raw(rx_ptr); }
                drop(lib); 
                
                tracing::info!("Task {} finished: {}", instance_id, res);
                res
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
            market_bridge,
            user_bridge,
        });
        
        Ok(info)
    }
    
    pub async fn stop(&self, instance_id: &str) -> Result<()> {
        if let Some((_, inst)) = self.instances.remove(instance_id) {
            inst.stop_flag.store(true, Ordering::Relaxed);
            inst.market_bridge.abort();
            if let Some(ub) = inst.user_bridge {
                ub.abort();
            }
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
}