// src/user_data/manager.rs

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use dashmap::DashMap;
use anyhow::Result;
use sha2::{Sha256, Digest};
use futures::{StreamExt, SinkExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

// Импортируем CEvent
use crate::ffi_types::{CEvent, CEventData};
// Импортируем наш новый парсер
use super::parser::parse_to_c_event;

const BINANCE_REST: &str = "https://fapi.binance.com";
const BINANCE_WS: &str = "wss://fstream.binance.com/ws";
const REFRESH_INTERVAL_SEC: u64 = 30 * 60; // 30 min

// ═══════════════════════════════════════════════════════════
// MANAGER STRUCTURES
// ═══════════════════════════════════════════════════════════

fn hash_key(api_key: &str) -> String {
    let mut h = Sha256::new();
    h.update(api_key.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StreamInfo {
    pub api_key_hash: String,
    pub connected: bool,
    pub connected_at: i64,
    pub last_event_at: Option<i64>,
    pub event_count: u64,
}

struct StreamHandle {
    #[allow(dead_code)]
    api_key: String,
    // Канал теперь передает CEvent
    event_tx: broadcast::Sender<CEvent>,
    shutdown: Arc<AtomicBool>,
    ws_task: JoinHandle<()>,
    refresh_task: JoinHandle<()>,
    
    // Stats
    connected_at: i64,
    event_count: Arc<AtomicU64>,
    last_event_at: Arc<AtomicU64>,
}

pub struct UserDataManager {
    streams: Arc<DashMap<String, StreamHandle>>,
    http: reqwest::Client,
}

impl UserDataManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            streams: Arc::new(DashMap::new()),
            http: reqwest::Client::new(),
        })
    }
    
    // ═══════════════════════════════════════════════════════════
    // PUBLIC API
    // ═══════════════════════════════════════════════════════════

    pub async fn connect(
        self: &Arc<Self>,
        api_key: String,
        _secret_key: String,
    ) -> Result<broadcast::Receiver<CEvent>> {
        let key_hash = hash_key(&api_key);
        
        if let Some(h) = self.streams.get(&key_hash) {
            tracing::info!("📡 Reusing stream for {}", key_hash);
            return Ok(h.event_tx.subscribe());
        }
        
        tracing::info!("📡 Connecting User Data Stream for {}", key_hash);
        
        let listen_key = self.get_listen_key(&api_key).await?;
        tracing::debug!("listenKey obtained: {}...", &listen_key[..10]);
        
        // Канал для CEvent
        let (event_tx, event_rx) = broadcast::channel(2048);
        let shutdown = Arc::new(AtomicBool::new(false));
        let event_count = Arc::new(AtomicU64::new(0));
        let last_event_at = Arc::new(AtomicU64::new(0));
        
        // WS Task
        let ws_task = {
            let manager = self.clone();
            let api_key = api_key.clone();
            let key_hash = key_hash.clone();
            let event_tx = event_tx.clone();
            let shutdown = shutdown.clone();
            let event_count = event_count.clone();
            let last_event_at = last_event_at.clone();
            let listen_key = listen_key.clone();
            
            tokio::spawn(async move {
                manager.ws_loop(
                    key_hash, api_key, listen_key,
                    event_tx, shutdown,
                    event_count, last_event_at
                ).await;
            })
        };
        
        // Refresh Task
        let refresh_task = {
            let manager = self.clone();
            let api_key = api_key.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                manager.refresh_loop(api_key, shutdown).await;
            })
        };
        
        self.streams.insert(key_hash.clone(), StreamHandle {
            api_key,
            event_tx,
            shutdown,
            ws_task,
            refresh_task,
            connected_at: chrono::Utc::now().timestamp(),
            event_count,
            last_event_at,
        });
        
        tracing::info!("✅ Stream connected for {}", key_hash);
        Ok(event_rx)
    }
    
    pub async fn disconnect(&self, api_key: &str) -> Result<()> {
        let key_hash = hash_key(api_key);
        if let Some((_, h)) = self.streams.remove(&key_hash) {
            tracing::info!("📴 Disconnecting stream {}", key_hash);
            h.shutdown.store(true, Ordering::Relaxed);
            h.ws_task.abort();
            h.refresh_task.abort();
            let _ = self.delete_listen_key(&h.api_key).await;
            Ok(())
        } else {
            anyhow::bail!("Stream not found")
        }
    }
    
    pub fn subscribe(&self, api_key: &str) -> Option<broadcast::Receiver<CEvent>> {
        let key_hash = hash_key(api_key);
        self.streams.get(&key_hash).map(|h| h.event_tx.subscribe())
    }
    
    pub fn list(&self) -> Vec<StreamInfo> {
        self.streams.iter().map(|e| {
            let h = e.value();
            StreamInfo {
                api_key_hash: e.key().clone(),
                connected: !h.shutdown.load(Ordering::Relaxed),
                connected_at: h.connected_at,
                last_event_at: {
                    let ts = h.last_event_at.load(Ordering::Relaxed);
                    if ts > 0 { Some(ts as i64) } else { None }
                },
                event_count: h.event_count.load(Ordering::Relaxed),
            }
        }).collect()
    }
    
    // ═══════════════════════════════════════════════════════════
    // WEBSOCKET LOOP
    // ═══════════════════════════════════════════════════════════
    
    async fn ws_loop(
        self: Arc<Self>,
        key_hash: String,
        api_key: String,
        mut listen_key: String,
        event_tx: broadcast::Sender<CEvent>,
        shutdown: Arc<AtomicBool>,
        event_count: Arc<AtomicU64>,
        last_event_at: Arc<AtomicU64>,
    ) {
        let mut retry_delay = 1u64;
        
        loop {
            if shutdown.load(Ordering::Relaxed) { break; }
            
            let url = format!("{}/{}", BINANCE_WS, listen_key);
            
            match connect_async(&url).await {
                Ok((ws, _)) => {
                    retry_delay = 1;
                    tracing::info!("🔌 WS connected [{}]", key_hash);
                    let (mut write, mut read) = ws.split();
                    
                    let shutdown_pong = shutdown.clone();
                    let pong_task = tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                            if shutdown_pong.load(Ordering::Relaxed) { break; }
                            if write.send(Message::Pong(vec![])).await.is_err() { break; }
                        }
                    });
                    
                    while let Some(msg) = read.next().await {
                        if shutdown.load(Ordering::Relaxed) { break; }
                        
                        match msg {
                            Ok(Message::Text(mut text)) => {
                                // Safety: simd_json требует мутабельный буфер
                                let bytes = unsafe { text.as_bytes_mut() };
                                
                                // Используем наш новый Zero-Copy парсер
                                if let Some(c_event) = parse_to_c_event(bytes) {
                                    let now = chrono::Utc::now().timestamp() as u64;
                                    last_event_at.store(now, Ordering::Relaxed);
                                    event_count.fetch_add(1, Ordering::Relaxed);
                                    
                                    // Логируем (опционально)
                                    if c_event.event_type == 2 { // Order
                                        let o = unsafe { &c_event.data.order };
                                        tracing::debug!("📦 Order {} {}", o.symbol_str(), o.order_id);
                                    }
                                    
                                    // Отправляем бинарное событие
                                    let _ = event_tx.send(c_event);
                                }
                            }
                            Ok(Message::Close(_)) => break,
                            Err(_) => break,
                            _ => {}
                        }
                    }
                    pong_task.abort();
                }
                Err(e) => tracing::error!("WS connect failed: {}", e),
            }
            
            if shutdown.load(Ordering::Relaxed) { break; }
            
            tracing::info!("Reconnecting in {}s...", retry_delay);
            tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay)).await;
            retry_delay = (retry_delay * 2).min(60);
            
            if let Ok(new_key) = self.get_listen_key(&api_key).await {
                listen_key = new_key;
            }
        }
    }

    async fn refresh_loop(self: Arc<Self>, api_key: String, shutdown: Arc<AtomicBool>) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(REFRESH_INTERVAL_SEC)).await;
            if shutdown.load(Ordering::Relaxed) { break; }
            if let Err(e) = self.keepalive_listen_key(&api_key).await {
                tracing::error!("Keepalive failed: {}", e);
            }
        }
    }
    
    async fn get_listen_key(&self, api_key: &str) -> Result<String> {
        let resp = self.http.post(format!("{}/fapi/v1/listenKey", BINANCE_REST))
            .header("X-MBX-APIKEY", api_key).send().await?;
        let json: serde_json::Value = resp.json().await?;
        json["listenKey"].as_str().map(String::from).ok_or_else(|| anyhow::anyhow!("No key"))
    }

    async fn keepalive_listen_key(&self, api_key: &str) -> Result<()> {
        let _ = self.http.put(format!("{}/fapi/v1/listenKey", BINANCE_REST))
            .header("X-MBX-APIKEY", api_key).send().await;
        Ok(())
    }
    
    async fn delete_listen_key(&self, api_key: &str) -> Result<()> {
        let _ = self.http.delete(format!("{}/fapi/v1/listenKey", BINANCE_REST))
            .header("X-MBX-APIKEY", api_key).send().await;
        Ok(())
    }
}