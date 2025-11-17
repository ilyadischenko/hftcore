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

// ─────────────────────────── События ───────────────────────────
#[derive(Debug, Clone)]
pub enum Event {
    Raw(Value),
}

// ─────────────────────────── Команды ───────────────────────────
#[derive(Debug, Clone)]
pub enum Command {
    SendLimitOrder(String, f64, f64, String),
    CancelLimitOrder(String, String),
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
    api_key: String,
    secret_key: String,

    is_connected: AtomicBool,

    // Очереди
    out_tx: mpsc::Sender<Outbound>,  // пользовательские сообщения
    ctrl_tx: mpsc::Sender<Ctrl>,     // служебные (Pong)

    pub event_tx: broadcast::Sender<Event>,

    // id -> callback (ждем ответ)
    pending: DashMap<String, Callback>,

    // Набор "отправлено в сокет, но нет ответа" — только ID, без payload.
    inflight_ids: DashSet<String>,

    id_counter: AtomicU64,
}

impl ExchangeTrade {
    pub fn new(ws_url: String, api_key: String, secret_key: String) -> Arc<Self> {
        let (out_tx, out_rx) = mpsc::channel::<Outbound>(8192);
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<Ctrl>(256);
        let (event_tx, _) = broadcast::channel::<Event>(2048);

        let mgr = Arc::new(Self {
            ws_url: ws_url.clone(),
            api_key,
            secret_key,
            is_connected: AtomicBool::new(false),
            out_tx,
            ctrl_tx,
            event_tx,
            pending: DashMap::new(),
            inflight_ids: DashSet::new(),
            id_counter: AtomicU64::new(0),
        });

        // WS-цикл
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
        // Сообщения, которые мы уже достали из out_rx, но так и не отправили в сокет (ошибка записи).
        // Они безопасно уйдут при следующем коннекте, это не реплей — они еще не уходили в сеть.
        let mut backlog: std::collections::VecDeque<Outbound> = std::collections::VecDeque::new();

        loop {
            tracing::info!("Trying to connect trade WS: {}", ws_url);
            match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((ws, _resp)) => {
                    tracing::info!("Connected to {}", ws_url);
                    self.is_connected.store(true, std::sync::atomic::Ordering::Relaxed);
                    let (mut write, mut read) = ws.split();

                    // Reader
                    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
                    let reader_mgr = self.clone();
                    let reader_ctrl_tx = self.ctrl_tx.clone();

                    tokio::spawn(async move {
                        loop {
                            match tokio::time::timeout(std::time::Duration::from_secs(30), read.next()).await {
                                Ok(Some(Ok(msg))) => {
                                    use tokio_tungstenite::tungstenite::Message;
                                    match msg {
                                        Message::Text(txt) => {
                                            reader_mgr.handle_text(txt).await;
                                        }
                                        Message::Binary(_bin) => {
                                            // при необходимости обработать
                                        }
                                        Message::Ping(data) => {
                                            // быстрый ответ Pong — без блокировки ридера
                                            let _ = reader_ctrl_tx.try_send(Ctrl::Pong(data));
                                        }
                                        Message::Pong(_) => {
                                            // ok
                                        }
                                        Message::Close(cf) => {
                                            tracing::warn!("WS close by server: {:?}", cf);
                                            break;
                                        }
                                        _ => {
                                            // покрывает Message::Frame(_) и возможные будущие варианты
                                        }
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
                                Err(_elapsed) => {
                                    // таймаут ожидания сообщения
                                    tracing::debug!("No messages for 30s (reader)");
                                }
                            }
                        }
                        let _ = done_tx.send(());
                    });

                    // Writer loop (+ ping). НИКАКОГО реплея отправленного ранее.
                    let mut ping_tick = tokio::time::interval(std::time::Duration::from_secs(15));
                    let mut connected = true;

                    // 1) Сначала пытаемся вывести backlog (эти сообщения не были отправлены ранее)
                    while let Some(ob) = backlog.pop_front() {
                        match write.send(tokio_tungstenite::tungstenite::Message::Text((*ob.payload).clone())).await {
                            Ok(_) => {
                                // Помечаем как реально отправленное — теперь ждём ответ
                                self.inflight_ids.insert(ob.id.clone());
                                tracing::trace!("Sent from backlog id={}", ob.id);
                            }
                            Err(e) => {
                                tracing::error!("WS send(backlog) error: {}", e);
                                // Возвращаем обратно и разрываем цикл — попробуем при следующем подключении
                                backlog.push_front(ob);
                                connected = false;
                                break;
                            }
                        }
                    }

                    // 2) Основной цикл записи
                    while connected {
                        tokio::select! {
                            _ = &mut done_rx => {
                                tracing::warn!("Reader finished — closing writer");
                                connected = false;
                            }

                            _ = ping_tick.tick() => {
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Ping(Vec::new())).await {
                                    tracing::error!("WS ping send error: {}", e);
                                    connected = false;
                                }
                            }

                            ctrl = ctrl_rx.recv() => {
                                if let Some(Ctrl::Pong(data)) = ctrl {
                                    if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await {
                                        tracing::error!("WS pong send error: {}", e);
                                        connected = false;
                                    }
                                } // если канал ctrl закрыт — просто игнорируем
                            }

                            msg = out_rx.recv() => {
                                match msg {
                                    Some(ob) => {
                                        match write.send(tokio_tungstenite::tungstenite::Message::Text((*ob.payload).clone())).await {
                                            Ok(_) => {
                                                // Ушло в сеть — ждём ответ по id
                                                self.inflight_ids.insert(ob.id.clone());
                                                tracing::trace!("Sent id={}", ob.id);
                                            }
                                            Err(e) => {
                                                tracing::error!("WS send error: {}", e);
                                                // Это сообщение НЕ было отправлено — складываем в backlog
                                                backlog.push_front(ob);
                                                connected = false;
                                            }
                                        }
                                    }
                                    None => {
                                        // out канал закрыт приложением — завершаем
                                        tracing::warn!("Outbound channel closed");
                                        connected = false;
                                    }
                                }
                            }
                        }
                    }

                    // Соединение закрыто: помечаем дисконнект и фейлим только реально отправленные (in-flight)
                    self.is_connected.store(false, std::sync::atomic::Ordering::Relaxed);

                    // Не пытаемся дожидаться done_rx ещё раз — он уже мог отработать в select.
                    self.fail_inflight_on_disconnect().await;

                    tracing::info!("Reconnecting in 2s...");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => {
                    self.is_connected.store(false, std::sync::atomic::Ordering::Relaxed);
                    tracing::error!("WS connect error: {e:?}, retrying in 2s");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
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
                        "error": { "code": "Disconnected", "message": "Connection closed before response" }
                    });
                    (cb)(v);
                });
            }
        }
    }

    // ─────────────────────────── Handle responses ───────────────────────────
    async fn handle_text(&self, txt: String) {
        // безопасный путь: парсим из &mut [u8]
        let mut bytes = txt.into_bytes();
        let parsed = simd_serde::from_slice::<Value>(&mut bytes)
            .or_else(|_e| serde_json::from_slice::<Value>(&bytes));

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

    // ─────────────────────────── Message builder ───────────────────────────
    fn build_message_for_cmd(&self, cmd: &Command, id: &str) -> Option<String> {
        let ts = Utc::now().timestamp_millis().to_string();
        let api_key = self.api_key.clone();
        let secret = self.secret_key.clone();

        match cmd {
            Command::SendLimitOrder(symbol, price, qty, side) => {
                let mut buf_price = Buffer::new();
                let mut buf_qty = Buffer::new();
                let price_str = buf_price.format(*price);
                let qty_str = buf_qty.format(*qty);

                let mut p: BTreeMap<&str, String> = BTreeMap::new();
                p.insert("apiKey", api_key.to_string());
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

                let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
                mac.update(query.as_bytes());
                let signature = hex::encode(mac.finalize().into_bytes());

                let mut params_json = serde_json::Map::new();
                params_json.insert("apiKey".into(), Value::String(api_key));
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

            Command::CancelLimitOrder(symbol, order_id) => {
                let mut p: BTreeMap<&str, String> = BTreeMap::new();
                p.insert("apiKey", api_key);
                p.insert("orderId", order_id.clone());
                p.insert("recvWindow", "5000".to_string());
                p.insert("symbol", symbol.to_uppercase());
                p.insert("timestamp", Utc::now().timestamp_millis().to_string());

                let query = p.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

                let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
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

    // ─────────────────────────── Public API ───────────────────────────

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

        // Регистрируем колбэк до отправки (чтобы не пропустить быстрый ответ)
        self.pending.insert(id.clone(), Arc::new(callback));

        // Отправляем в writer-очередь (await — backpressure, без блокировки потока)
        if let Err(e) = self.out_tx.send(Outbound { id, payload }).await {
            tracing::error!("Outbound channel send error: {}", e);
            // Канал закрыт приложением — pending останется, но ответ уже не придет.
        }
    }

    pub async fn send_limit_order<F>(
        &self,
        symbol: &str,
        price: f64,
        size: f64,
        side: &str,
        callback: F,
    ) where
        F: Fn(Value) + Send + Sync + 'static,
    {
        self.send_command(
            Command::SendLimitOrder(symbol.into(), price, size, side.into()),
            callback,
        )
        .await;
    }

    pub async fn cancel_limit_order<F>(
        &self,
        symbol: &str,
        order_id: &str,
        callback: F,
    ) where
        F: Fn(Value) + Send + Sync + 'static,
    {
        self.send_command(
            Command::CancelLimitOrder(symbol.into(), order_id.into()),
            callback,
        )
        .await;
    }
}