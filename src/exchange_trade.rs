use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use ryu::Buffer;
use serde_json::{json, Value};
use simd_json::serde as simd_serde;
use tokio::{
    select,
    sync::{broadcast, mpsc, oneshot},
    time::{interval, sleep, timeout, Duration},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use dashmap::{DashMap, DashSet};
use sha2::Sha256;
type HmacSha256 = Hmac<Sha256>;

use std::sync::atomic::AtomicI64;

// ─────────────────────────── События ───────────────────────────
#[derive(Debug, Clone)]
pub enum Event {
    Raw(Value),
}

// ─────────────────────────── Команды ───────────────────────────
#[derive(Debug, Clone)]
pub enum Command {
    SendLimitOrder {
        api_key: String,
        secret_key: String,
        symbol: String,
        price: f64,
        qty: f64,
        side: String,
    },
    CancelLimitOrder {
        api_key: String,
        secret_key: String,
        symbol: String,
        order_id: String,
    },
}

// ─────────────────────────── Внутренние типы ───────────────────────────
type Callback = Arc<dyn Fn(Value) + Send + Sync + 'static>;
type SharedStr = Arc<String>;

#[derive(Debug)]
struct Outbound {
    id: String,
    payload: SharedStr,
}

#[derive(Debug)]
enum Ctrl {
    Pong(Vec<u8>),
}

pub struct ExchangeTrade {
    ws_url: String,
    // ← УБРАЛИ api_key и secret_key

    is_connected: AtomicBool,

    // Очереди
    out_tx: mpsc::Sender<Outbound>,
    ctrl_tx: mpsc::Sender<Ctrl>,

    pub event_tx: broadcast::Sender<Event>,

    pending: DashMap<String, Callback>,
    inflight_ids: DashSet<String>,
    id_counter: AtomicU64,
    
    time_offset_ms: AtomicI64,
}

impl ExchangeTrade {
    // ═══════════════════════════════════════════════════════════
    // ИЗМЕНЕНИЕ: new() БЕЗ api_key и secret_key
    // ═══════════════════════════════════════════════════════════
    pub fn new(ws_url: String) -> Arc<Self> {
        let (out_tx, out_rx) = mpsc::channel::<Outbound>(8192);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<Ctrl>(256);
        let (event_tx, _) = broadcast::channel::<Event>(2048);

        let mgr = Arc::new(Self {
            ws_url: ws_url.clone(),
            // ← УБРАЛИ
            is_connected: AtomicBool::new(false),
            out_tx,
            ctrl_tx,
            event_tx,
            pending: DashMap::new(),
            inflight_ids: DashSet::new(),
            id_counter: AtomicU64::new(0),
            time_offset_ms: AtomicI64::new(0),
        });

        {
            let mgr_clone = mgr.clone();
            tokio::spawn(async move {
                mgr_clone.run_socket(ws_url, out_rx, ctrl_rx).await;
            });
        }

        mgr
    }

    fn next_id(&self) -> String {
        let n = self.id_counter.fetch_add(1, Ordering::Relaxed) + 1;
        format!("req-{n}")
    }

    async fn run_socket(
        self: Arc<Self>,
        ws_url: String,
        mut out_rx: mpsc::Receiver<Outbound>,
        mut ctrl_rx: mpsc::Receiver<Ctrl>,
    ) {
        let mut backlog: VecDeque<Outbound> = VecDeque::new();

        loop {
            tracing::info!("Trying to connect trade WS: {}", ws_url);
            match connect_async(&ws_url).await {
                Ok((ws, _resp)) => {
                    tracing::info!("Connected to {}", ws_url);
                    self.is_connected.store(true, Ordering::Relaxed);
                    let (mut write, mut read) = ws.split();

                    let (done_tx, mut done_rx) = oneshot::channel::<()>();
                    let reader_mgr = self.clone();
                    let reader_ctrl_tx = self.ctrl_tx.clone();

                    tokio::spawn(async move {
                        loop {
                            match timeout(Duration::from_secs(30), read.next()).await {
                                Ok(Some(Ok(msg))) => {
                                    match msg {
                                        Message::Text(txt) => {
                                            reader_mgr.handle_text(txt).await;
                                        }
                                        Message::Binary(_) => {}
                                        Message::Ping(data) => {
                                            let _ = reader_ctrl_tx.try_send(Ctrl::Pong(data));
                                        }
                                        Message::Pong(_) => {}
                                        Message::Close(cf) => {
                                            tracing::warn!("WS close: {:?}", cf);
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                                Ok(Some(Err(e))) => {
                                    tracing::error!("WS read error: {}", e);
                                    break;
                                }
                                Ok(None) => {
                                    tracing::warn!("WS stream ended");
                                    break;
                                }
                                Err(_) => {}
                            }
                        }
                        let _ = done_tx.send(());
                    });

                    let mut ping_tick = interval(Duration::from_secs(15));
                    let mut connected = true;

                    while let Some(ob) = backlog.pop_front() {
                        match write.send(Message::Text((*ob.payload).clone())).await {
                            Ok(_) => {
                                self.inflight_ids.insert(ob.id.clone());
                            }
                            Err(e) => {
                                tracing::error!("WS send(backlog) error: {}", e);
                                backlog.push_front(ob);
                                connected = false;
                                break;
                            }
                        }
                    }

                    while connected {
                        select! {
                            _ = &mut done_rx => {
                                tracing::warn!("Reader finished");
                                connected = false;
                            }

                            _ = ping_tick.tick() => {
                                if let Err(e) = write.send(Message::Ping(Vec::new())).await {
                                    tracing::error!("WS ping error: {}", e);
                                    connected = false;
                                }
                            }

                            ctrl = ctrl_rx.recv() => {
                                if let Some(Ctrl::Pong(data)) = ctrl {
                                    if let Err(e) = write.send(Message::Pong(data)).await {
                                        tracing::error!("WS pong error: {}", e);
                                        connected = false;
                                    }
                                }
                            }

                            msg = out_rx.recv() => {
                                match msg {
                                    Some(ob) => {
                                        match write.send(Message::Text((*ob.payload).clone())).await {
                                            Ok(_) => {
                                                self.inflight_ids.insert(ob.id.clone());
                                            }
                                            Err(e) => {
                                                tracing::error!("WS send error: {}", e);
                                                backlog.push_front(ob);
                                                connected = false;
                                            }
                                        }
                                    }
                                    None => {
                                        tracing::warn!("Outbound channel closed");
                                        connected = false;
                                    }
                                }
                            }
                        }
                    }

                    self.is_connected.store(false, Ordering::Relaxed);
                    self.fail_inflight_on_disconnect().await;

                    tracing::info!("Reconnecting in 2s...");
                    sleep(Duration::from_secs(2)).await;
                }
                Err(e) => {
                    self.is_connected.store(false, Ordering::Relaxed);
                    tracing::error!("WS connect error: {:?}", e);
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn fail_inflight_on_disconnect(&self) {
        let ids: Vec<String> = self.inflight_ids.iter().map(|id| id.clone()).collect();
        for id in ids {
            self.inflight_ids.remove(&id);
            if let Some((_k, cb)) = self.pending.remove(&id) {
                let id_cl = id.clone();
                tokio::spawn(async move {
                    let v = json!({
                        "id": id_cl,
                        "error": { "code": "Disconnected", "message": "Connection closed" }
                    });
                    (cb)(v);
                });
            }
        }
    }

    async fn handle_text(&self, txt: String) {
        let mut bytes = txt.into_bytes();
        let parsed = simd_serde::from_slice::<Value>(&mut bytes)
            .or_else(|_| serde_json::from_slice::<Value>(&bytes));

        let v = match parsed {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("JSON parse error: {}", e);
                return;
            }
        };

        if let Some(id) = Self::extract_id(&v) {
            if self.inflight_ids.remove(&id).is_some() {
                tracing::trace!("Ack for id={}", id);
            }
            if let Some((_k, cb)) = self.pending.remove(&id) {
                tokio::spawn(async move {
                    (cb)(v);
                });
                return;
            }
        }

        let _ = self.event_tx.send(Event::Raw(v));
    }

    fn extract_id(v: &Value) -> Option<String> {
        match v.get("id") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    // ═══════════════════════════════════════════════════════════
    // ИЗМЕНЕНИЕ: берем ключи из команды
    // ═══════════════════════════════════════════════════════════
    fn build_message_for_cmd(&self, cmd: &Command, id: &str) -> Option<String> {
        let local_time_ms = Utc::now().timestamp_millis();
        
        // Применяем offset (может быть положительным или отрицательным)
        let offset = self.time_offset_ms.load(Ordering::Relaxed);
        let adjusted_time_ms = local_time_ms + offset;
        
        let ts = adjusted_time_ms.to_string();

        match cmd {
            Command::SendLimitOrder { api_key, secret_key, symbol, price, qty, side } => {
                let mut buf_price = Buffer::new();
                let mut buf_qty = Buffer::new();
                let price_str = buf_price.format(*price);
                let qty_str = buf_qty.format(*qty);

                let mut p: BTreeMap<&str, String> = BTreeMap::new();
                p.insert("apiKey", api_key.clone());
                p.insert("positionSide", "BOTH".to_string());
                p.insert("price", price_str.to_string());
                p.insert("quantity", qty_str.to_string());
                p.insert("recvWindow", "5000".to_string());
                p.insert("side", side.to_uppercase());
                p.insert("symbol", symbol.to_uppercase());
                p.insert("timeInForce", "GTC".to_string());
                p.insert("timestamp", ts.clone());
                p.insert("type", "LIMIT".to_string());

                let query = p.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

                let mut mac = HmacSha256::new_from_slice(secret_key.as_bytes()).ok()?;
                mac.update(query.as_bytes());
                let signature = hex::encode(mac.finalize().into_bytes());

                let mut params_json = serde_json::Map::new();
                params_json.insert("apiKey".into(), Value::String(api_key.clone()));
                params_json.insert("positionSide".into(), Value::String("BOTH".into()));
                params_json.insert("price".into(), Value::Number(serde_json::Number::from_f64(*price)?));
                params_json.insert("quantity".into(), Value::Number(serde_json::Number::from_f64(*qty)?));
                params_json.insert("recvWindow".into(), Value::String("5000".into()));
                params_json.insert("side".into(), Value::String(side.to_uppercase()));
                params_json.insert("symbol".into(), Value::String(symbol.to_uppercase()));
                params_json.insert("timeInForce".into(), Value::String("GTC".into()));
                params_json.insert("timestamp".into(), Value::String(ts));
                params_json.insert("type".into(), Value::String("LIMIT".into()));
                params_json.insert("signature".into(), Value::String(signature));

                let msg_json = json!({
                    "id": id,
                    "method": "order.place",
                    "params": params_json
                });

                Some(msg_json.to_string())
            }

            Command::CancelLimitOrder { api_key, secret_key, symbol, order_id } => {
                let mut p: BTreeMap<&str, String> = BTreeMap::new();
                p.insert("apiKey", api_key.clone());
                p.insert("orderId", order_id.clone());
                p.insert("recvWindow", "5000".to_string());
                p.insert("symbol", symbol.to_uppercase());
                p.insert("timestamp", ts);

                let query = p.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

                let mut mac = HmacSha256::new_from_slice(secret_key.as_bytes()).ok()?;
                mac.update(query.as_bytes());
                let sig_hex = hex::encode(mac.finalize().into_bytes());

                let mut params_json = serde_json::Map::new();
                for (k, v) in p.into_iter() {
                    params_json.insert(k.to_string(), Value::String(v));
                }
                params_json.insert("signature".to_string(), Value::String(sig_hex));

                let msg_json = json!({
                    "id": id,
                    "method": "order.cancel",
                    "params": Value::Object(params_json)
                });
                Some(msg_json.to_string())
            }
        }
    }

    // ═══════════════════════════════════════════════════════════
    // PUBLIC API
    // ═══════════════════════════════════════════════════════════

    pub async fn send_command<F>(&self, cmd: Command, callback: F)
    where
        F: Fn(Value) + Send + Sync + 'static,
    {
        let id = self.next_id();
        let Some(payload_str) = self.build_message_for_cmd(&cmd, &id) else {
            tracing::error!("Build message failed for id={}", id);
            return;
        };

        let payload: SharedStr = Arc::new(payload_str);

        self.pending.insert(id.clone(), Arc::new(callback));

        if let Err(e) = self.out_tx.send(Outbound { id, payload }).await {
            tracing::error!("Outbound channel send error: {}", e);
        }
    }

    // ═══════════════════════════════════════════════════════════
    // ИЗМЕНЕНИЕ: send_limit_order принимает api_key и secret_key
    // ═══════════════════════════════════════════════════════════
    pub async fn send_limit_order<F>(
        &self,
        api_key: &str,
        secret_key: &str,
        symbol: &str,
        price: f64,
        size: f64,
        side: &str,
        callback: F,
    ) where
        F: Fn(Value) + Send + Sync + 'static,
    {
        self.send_command(
            Command::SendLimitOrder {
                api_key: api_key.to_string(),
                secret_key: secret_key.to_string(),
                symbol: symbol.to_string(),
                price,
                qty: size,
                side: side.to_string(),
            },
            callback,
        )
        .await;
    }

    pub async fn cancel_limit_order<F>(
        &self,
        api_key: &str,
        secret_key: &str,
        symbol: &str,
        order_id: &str,
        callback: F,
    ) where
        F: Fn(Value) + Send + Sync + 'static,
    {
        self.send_command(
            Command::CancelLimitOrder {
                api_key: api_key.to_string(),
                secret_key: secret_key.to_string(),
                symbol: symbol.to_string(),
                order_id: order_id.to_string(),
            },
            callback,
        )
        .await;
    }

        // ═══════════════════════════════════════════════════════════
    // НОВЫЙ МЕТОД: синхронизация времени с Binance
    // ═══════════════════════════════════════════════════════════
    
    /// Синхронизирует локальное время с сервером Binance
    /// 
    /// Алгоритм:
    /// 1. Запрашивает GET https://fapi.binance.com/fapi/v1/time
    /// 2. Получает {"serverTime": 1700000000000}
    /// 3. Вычисляет offset = server_time - local_time
    /// 4. Сохраняет offset в AtomicI64
    /// 
    /// Пример:
    /// - Local time:  1700000001500 (ваше время)
    /// - Server time: 1700000000000 (Binance время)
    /// - Offset:      -1500ms       (на столько уменьшаем timestamp в запросах)
    pub async fn sync_time(&self) -> anyhow::Result<i64> {
        tracing::info!("⏰ Syncing time with Binance server...");
        
        // Запоминаем время ДО запроса (для учёта network latency)
        let t0 = Utc::now().timestamp_millis();
        
        // Выполняем HTTP GET запрос
        let client = reqwest::Client::new();
        let resp = client
            .get("https://fapi.binance.com/fapi/v1/time")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("HTTP request failed: {}", e))?;
        
        // Время ПОСЛЕ получения ответа
        let t1 = Utc::now().timestamp_millis();
        
        // Парсим JSON: {"serverTime": 1700000000000}
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("JSON parse failed: {}", e))?;
        
        // Извлекаем serverTime
        let server_time = json["serverTime"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("Missing serverTime in response"))?;
        
        // ═══════════════════════════════════════════════════════════
        // ВЫЧИСЛЕНИЕ OFFSET с учётом сетевой задержки
        // ═══════════════════════════════════════════════════════════
        
        // Сетевая задержка (round-trip time)
        let network_delay = t1 - t0;
        
        // Предполагаем что задержка симметрична: request_time ≈ response_time
        // Значит серверное время соответствует середине интервала
        let local_time_when_server_responded = t0 + network_delay / 2;
        
        // Offset = сколько нужно ДОБАВИТЬ к локальному времени чтобы получить серверное
        let offset = server_time - local_time_when_server_responded;
        
        // Сохраняем в AtomicI64 (thread-safe)
        self.time_offset_ms.store(offset, Ordering::Relaxed);
        
        tracing::info!(
            "✅ Time synchronized | local={} server={} offset={}ms latency={}ms",
            local_time_when_server_responded,
            server_time,
            offset,
            network_delay
        );
        
        Ok(offset)
    }
    
    /// Установить offset вручную (для тестирования)
    pub fn set_time_offset(&self, offset_ms: i64) {
        self.time_offset_ms.store(offset_ms, Ordering::Relaxed);
        tracing::info!("⏰ Time offset manually set to {}ms", offset_ms);
    }
    
    /// Получить текущий offset
    pub fn get_time_offset(&self) -> i64 {
        self.time_offset_ms.load(Ordering::Relaxed)
    }
}