// src/strategies/runner.rs

use libloading::Library;
use tokio::sync::broadcast; // Для динамической загрузки библиотек (.so, .dll).
use std::path::PathBuf; // Для работы с путями файловой системы.
use std::sync::Arc; // Atomic Reference Counter - умный указатель для многопоточности.
// use std::sync::atomic::{AtomicBool, Ordering}; // AtomicBool: Булево значение которое можно безопасно менять из разных потоков.
use tokio::task::JoinHandle; // Handle на запущенный async task, через него можем дождаться завершения.
use dashmap::DashMap; // Thread-safe HashMap (можно читать/писать из разных потоков одновременно).
use anyhow::Result;
use crossbeam::channel::{bounded, Receiver};
use crate::exchange_trade::Event;
use crate::ffi_types::CEvent;

type RunFn = unsafe extern "C" fn(rx: *mut Receiver<CEvent>) -> i32;
type StopFn = unsafe extern "C" fn();
// Разбор:
// extern "C" - функция использует C calling convention (совместимо с FFI)
// fn() -> i32 - функция без аргументов, возвращает int
// unsafe - вызов требует unsafe блок


pub struct StrategyRunner {
    running: DashMap<String, RunningStrategy>,
}
// Что хранит: Карту "ID стратегии" → "Информация о запущенной стратегии".
// Обращения к этой области памяти будут приходить из разных потоков, поэтому используем DashMap. Thread-safe хранилище

struct RunningStrategy {
    _lib: Arc<Library>, // Держим библиотеку в памяти пока стратегия запущена. В arc чтобы можно было клонировать в поток.
    stop_fn: StopFn, // Флаг остановки стратегии. Прокидывается в стратегию извне. В атомике чтобы было доступно извне.
    task: JoinHandle<()>, // Хэндл на задачу, чтобы можно было дождаться её завершения.
    bridge_task: JoinHandle<()>,  // ← новое поле для bridge
}
// Инфа о запущенной стратегии

impl StrategyRunner {
    pub fn new() -> Arc<Self> {
        // Создаем новый экземпляр StrategyRunner с пустой DashMap
        Arc::new(Self {
            running: DashMap::new(),
        })
    }
    
// src/strategies/runner.rs (исправленная версия)

    pub async fn start(
        &self,
        strategy_id: String,
        lib_path: PathBuf,
        mut event_rx: broadcast::Receiver<CEvent>,
    ) -> Result<()> {
        if self.running.contains_key(&strategy_id) {
            anyhow::bail!("Strategy '{}' is already running", strategy_id);
        }
        
        tracing::info!("📦 Loading library: {:?}", lib_path);
        
        let lib: Arc<Library> = Arc::new(unsafe { 
            Library::new(&lib_path)?
        });
        
        let (sync_tx, sync_rx) = bounded::<CEvent>(8192);
        
        // Bridge task
        let strategy_id_clone = strategy_id.clone();
        let bridge_task = tokio::spawn(async move {
            let mut dropped = 0;
            
            while let Ok(event) = event_rx.recv().await {
                match sync_tx.try_send(event) {
                    Ok(_) => {},
                    Err(crossbeam::channel::TrySendError::Full(_)) => {
                        dropped += 1;
                        if dropped % 1000 == 0 {
                            tracing::warn!(
                                "⚠️ Strategy '{}' lagging: {} dropped",
                                strategy_id_clone,
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
        let strategy_id_clone = strategy_id.clone();
        
        // ═══════════════════════════════════════════════════════════
        // ИСПРАВЛЕНИЕ: передаём SAM Receiver, а не указатель!
        // ═══════════════════════════════════════════════════════════
        
        let task = tokio::task::spawn_blocking(move || {
            tracing::info!("🚀 Calling run() for '{}'...", strategy_id_clone);
            
            // Создаём указатель ЗДЕСЬ, внутри потока
            let rx_ptr = Box::into_raw(Box::new(sync_rx));
            
            // Вызываем функцию стратегии
            let result = unsafe { run_fn(rx_ptr) };
            
            tracing::info!("✅ run() returned {} for '{}'", result, strategy_id_clone);
            
            // Очищаем память
            unsafe { 
                let _ = Box::from_raw(rx_ptr); 
            }
            
            drop(lib_clone);
        });
        
        self.running.insert(strategy_id.clone(), RunningStrategy {
            _lib: lib,
            stop_fn,
            task,
            bridge_task,
        });
        
        tracing::info!("✅ Strategy '{}' started", strategy_id);
        Ok(())
    }
    /// Остановить стратегию
    /// 
    /// # Аргументы
    /// * `strategy_id` - ID стратегии для остановки
    pub async fn stop(&self, strategy_id: &str) -> Result<()> {
        // Удаляем из карты запущенных
        let (_, running) = self.running.remove(strategy_id)
            .ok_or_else(|| anyhow::anyhow!("Strategy '{}' not running", strategy_id))?;
        
        tracing::info!("🛑 Stopping strategy '{}'...", strategy_id);
        
        // ═══════════════════════════════════════════════════════════
        // 1. Вызываем stop() из DLL
        // ═══════════════════════════════════════════════════════════
        
        tracing::debug!("Calling stop() function from DLL...");
        unsafe {
            (running.stop_fn)();
        }
        
        // ═══════════════════════════════════════════════════════════
        // 2. Останавливаем bridge task
        // ═══════════════════════════════════════════════════════════
        
        running.bridge_task.abort();
        
        // ═══════════════════════════════════════════════════════════
        // 3. Ждём завершения стратегии (с таймаутом)
        // ═══════════════════════════════════════════════════════════
        
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            running.task
        ).await {
            Ok(Ok(())) => {
                tracing::info!("✅ Strategy '{}' stopped cleanly", strategy_id);
                Ok(())
            }
            Ok(Err(e)) => {
                tracing::error!("❌ Strategy '{}' task panicked: {:?}", strategy_id, e);
                anyhow::bail!("Strategy task panicked: {:?}", e)
            }
            Err(_) => {
                tracing::warn!("⚠️ Strategy '{}' didn't stop in 5s (might be hanging)", strategy_id);
                // Не возвращаем ошибку - библиотека уже unloaded
                Ok(())
            }
        }
    }
    
    pub fn list_running(&self) -> Vec<String> {
        self.running.iter().map(|e| e.key().clone()).collect()
    }
    
    pub fn is_running(&self, strategy_id: &str) -> bool {
        self.running.contains_key(strategy_id)
    }
}