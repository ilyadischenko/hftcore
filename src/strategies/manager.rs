// src/strategies/manager.rs

use libloading::Library;
use tokio::sync::broadcast;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinHandle;
use dashmap::DashMap;
use anyhow::Result;
use crossbeam::channel::{bounded, Receiver};
use crate::ffi_types::CEvent;
use std::ffi::CString;

use crate::strategies::order::{
    OrderResult, OrderCallback, PlaceOrderFn, CancelOrderFn,
    place_order, cancel_order,
};

// ═══════════════════════════════════════════════════════════
// КОНФИГ С JSON ПАРАМЕТРАМИ
// ═══════════════════════════════════════════════════════════

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StrategyConfig {
    pub symbol: [u8; 16],
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

pub struct StrategyRunner {
    running: DashMap<String, RunningStrategy>,
}

struct RunningStrategy {
    _lib: Arc<Library>,
    stop_fn: StopFn,
    task: JoinHandle<()>,
    bridge_task: JoinHandle<()>,
}

impl StrategyRunner {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            running: DashMap::new(),
        })
    }
    
    pub async fn start(
        &self,
        instance_id: String,
        lib_path: PathBuf,
        mut event_rx: broadcast::Receiver<CEvent>,
        symbol: String,
        params_json: String,  // ← String, не указатель
    ) -> Result<()> {
        if self.running.contains_key(&instance_id) {
            anyhow::bail!("Instance '{}' is already running", instance_id);
        }
        
        tracing::info!("📦 Loading library for instance '{}' with params: {}", 
                       instance_id, params_json);
        
        let lib: Arc<Library> = Arc::new(unsafe { 
            Library::new(&lib_path)?
        });
        
        let (sync_tx, sync_rx) = bounded::<CEvent>(8192);
        
        // Bridge task
        let instance_id_clone = instance_id.clone();
        let bridge_task = tokio::spawn(async move {
            let mut dropped = 0;
            
            while let Ok(event) = event_rx.recv().await {
                match sync_tx.try_send(event) {
                    Ok(_) => {},
                    Err(crossbeam::channel::TrySendError::Full(_)) => {
                        dropped += 1;
                        if dropped % 1000 == 0 {
                            tracing::warn!(
                                "⚠️ Instance '{}' lagging: {} dropped",
                                instance_id_clone,
                                dropped
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        
        // Загружаем функции
        let run_fn: RunFn = unsafe {
            let symbol: libloading::Symbol<RunFn> = lib.get(b"run")?;
            *symbol
        };
        
        let stop_fn: StopFn = unsafe {
            let symbol = lib.get(b"stop")?;
            *symbol
        };
        
        let lib_clone = lib.clone();
        let instance_id_clone = instance_id.clone();
        
        // ═══════════════════════════════════════════════════════════
        // ИСПРАВЛЕНИЕ: передаём String, CString создаём ВНУТРИ blocking thread
        // ═══════════════════════════════════════════════════════════
        
        let task = tokio::task::spawn_blocking(move || {
            tracing::info!("🚀 Starting instance '{}'...", instance_id_clone);
            
            // Создаём CString ЗДЕСЬ, внутри blocking thread
            let params_cstring = CString::new(params_json)
                .expect("Invalid JSON string");
            
            // Подготовка symbol
            let mut symbol_bytes = [0u8; 16];
            let bytes = symbol.as_bytes();
            let len = bytes.len().min(15);
            symbol_bytes[..len].copy_from_slice(&bytes[..len]);
            
            // Создаём конфиг с указателем на CString
            let config = StrategyConfig {
                symbol: symbol_bytes,
                symbol_len: len as u8,
                params_json: params_cstring.as_ptr(),
            };
            
            let rx_ptr = Box::into_raw(Box::new(sync_rx));
            
            // Вызываем функцию стратегии
            let result = unsafe { 
                run_fn(
                    rx_ptr,
                    place_order,
                    cancel_order,
                    config,
                )
            };
            
            tracing::info!("✅ Instance '{}' exited with code {}", instance_id_clone, result);
            
            // Очищаем память
            unsafe { 
                let _ = Box::from_raw(rx_ptr); 
            }
            
            // params_cstring дропнется автоматически здесь
            drop(lib_clone);
        });
        
        self.running.insert(instance_id.clone(), RunningStrategy {
            _lib: lib,
            stop_fn,
            task,
            bridge_task,
        });
        
        tracing::info!("✅ Instance '{}' started", instance_id);
        Ok(())
    }
    
    pub async fn stop(&self, instance_id: &str) -> Result<()> {
        let (_, running) = self.running.remove(instance_id)
            .ok_or_else(|| anyhow::anyhow!("Instance '{}' not running", instance_id))?;
        
        tracing::info!("🛑 Stopping instance '{}'...", instance_id);
        
        unsafe {
            (running.stop_fn)();
        }
        
        running.bridge_task.abort();
        
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            running.task
        ).await {
            Ok(Ok(())) => {
                tracing::info!("✅ Instance '{}' stopped cleanly", instance_id);
                Ok(())
            }
            Ok(Err(e)) => {
                tracing::error!("❌ Instance '{}' task panicked: {:?}", instance_id, e);
                anyhow::bail!("Instance task panicked: {:?}", e)
            }
            Err(_) => {
                tracing::warn!("⚠️ Instance '{}' didn't stop in 5s", instance_id);
                Ok(())
            }
        }
    }
    
    pub fn list_running(&self) -> Vec<String> {
        self.running.iter().map(|e| e.key().clone()).collect()
    }
    
    pub fn is_running(&self, instance_id: &str) -> bool {
        self.running.contains_key(instance_id)
    }
}