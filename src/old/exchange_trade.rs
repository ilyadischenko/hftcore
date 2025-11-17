use tokio::{
    sync::{mpsc, Mutex},
    time::{sleep, Duration, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio::sync::broadcast;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use simd_json::serde as simd_serde;
use std::{sync::Arc, collections::HashMap};
use chrono::Utc;

use std::collections::BTreeMap;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use ryu::Buffer;
type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub enum Event {
    // BookTicker(BookTicker),
    // Trade(Trade),
}

// ─────────────────────────── Команды ───────────────────────────
#[derive(Debug, Clone)]
pub enum Command {
    SendLimitOrder(String, f64, f64, String),
    CancelLimitOrder(String, String),
}

// ─────────────────────────── Менеджер ───────────────────────────
pub struct ExchangeTrade {
    ws_url: String,
    api_key: String,
    secret_key: String,
    is_connected: Arc<Mutex<bool>>,
    cmd_tx: mpsc::Sender<Command>,
    pub event_tx: broadcast::Sender<Event>,
    // id -> callback с ответом
    pending: Arc<Mutex<HashMap<String, Box<dyn Fn(Value) + Send + Sync>>>>,
    id_counter: Arc<Mutex<u64>>,
}

impl ExchangeTrade {
    pub fn new(ws_url: String, api_key: String, secret_key: String) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (event_tx, _) = broadcast::channel(1024);
        let mgr = Arc::new(Self {
            ws_url: ws_url.clone(),
            api_key,
            secret_key,
            is_connected: Arc::new(Mutex::new(false)),
            cmd_tx,
            event_tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            id_counter: Arc::new(Mutex::new(0)),
        });

        let clone = mgr.clone();
        tokio::spawn(async move {
            clone.run_socket(ws_url, cmd_rx).await;
        });

        mgr
    }

    async fn next_id(&self) -> String {
        let mut g = self.id_counter.lock().await;
        *g += 1;
        format!("req-{}", *g)
    }

    async fn run_socket(self: Arc<Self>, ws_url: String, mut cmd_rx: mpsc::Receiver<Command>) {
        loop {
            tracing::info!("Trying to connect trade...");
            match connect_async(&ws_url).await {
                Ok((mut ws, _)) => {
                    tracing::info!("Connected to {ws_url}");
                    *self.is_connected.lock().await = true;

                    let (mut write, mut read) = ws.split();
                    let reader_mgr = self.clone();

                    // ─── Reader task ───
                    tokio::spawn(async move {
                        loop {
                            match timeout(Duration::from_secs(30), read.next()).await {
                                Ok(Some(Ok(Message::Text(txt)))) => {
                                    println!("📥 RAW MESSAGE (before parse): {txt}");
                                    reader_mgr.handle_text(txt).await;
                                }
                                Ok(Some(Ok(Message::Close(f)))) => {
                                    tracing::warn!("Server closed socket: {:?}", f);
                                    *reader_mgr.is_connected.lock().await = false;
                                    break;
                                }
                                Ok(Some(Err(e))) => {
                                    tracing::error!("Read error: {e}");
                                    *reader_mgr.is_connected.lock().await = false;
                                    break;
                                }
                                Ok(None) => {
                                    tracing::warn!("Stream ended");
                                    *reader_mgr.is_connected.lock().await = false;
                                    break;
                                }
                                Err(_) => {
                                    tracing::warn!("No messages for 30s — still alive check");
                                }
                                _ => {}
                            }
                        }
                    });

                    // ─── Writer task ───
                    while let Some(cmd) = cmd_rx.recv().await {
                        if !*self.is_connected.lock().await {
                            tracing::warn!("Not connected, skip command");
                            sleep(Duration::from_millis(200)).await;
                            continue;
                        }

                        let (id, msg) = self.build_message_with_id(&cmd).await;
                        if msg.is_empty() {
                            tracing::warn!("Command skipped due to invalid key or build error");
                            continue;
                        }

                        if let Err(e) = write.send(Message::Text(msg)).await {
                            tracing::error!("Send failed: {e}");
                            *self.is_connected.lock().await = false;
                            break;
                        }

                        tracing::debug!("Sent command id={id}");
                    }

                    tracing::info!("Reconnecting in 3 s...");
                    sleep(Duration::from_secs(3)).await;
                }
                Err(e) => {
                    *self.is_connected.lock().await = false;
                    tracing::error!("WS connect error: {e:?}, retrying");
                    sleep(Duration::from_secs(3)).await;
                }
            }
        }
    }

    // ─────────────────────────── Handle responses ───────────────────────────
    async fn handle_text(&self, txt: String) {
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            if let Some(id_str) = v.get("id").and_then(|x| x.as_str()) {
                let mut pending = self.pending.lock().await;
                if let Some(cb) = pending.remove(id_str) {
                    drop(pending);
                    cb(v);
                    return;
                }
            }
            // ...
        } else {
            let _ = Self::handle_system_message(txt.as_str());
        }
    }

    fn handle_system_message(raw: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut tmp = raw.to_string();
        println!("📩 RAW WS MESSAGE:\n{raw}\n──────────────────────────────");
        let v: Value = unsafe { simd_serde::from_str(&mut tmp)? };
        if let Some(result) = v.get("result") {
            tracing::info!("WS result: {result}");
        } else if let Some(code) = v.get("code") {
            tracing::error!("WS error code {code:?}: {:?}", v.get("msg"));
        } else {
            tracing::debug!("WS system msg: {}", v.to_string());
        }
        Ok(())
    }

    // ─────────────────────────── Message builder ───────────────────────────


    async fn build_message_with_id(&self, cmd: &Command) -> (String, String) {
        let id = self.next_id().await;
        let ts = chrono::Utc::now().timestamp_millis().to_string();
        let api_key = self.api_key.clone();
        let secret = self.secret_key.clone();

        match cmd {
            Command::SendLimitOrder(symbol, price, qty, side) => {
            // компактное, быстрое и корректное f64→string
                let mut buf_price = Buffer::new();
                let mut buf_qty = Buffer::new();
                let price_str = buf_price.format(*price);
                let qty_str = buf_qty.format(*qty);

                // формируем параметры для подписи
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

                // формируем query для подписи
                let query = p.iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join("&");

                let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC key");
                mac.update(query.as_bytes());
                let signature = hex::encode(mac.finalize().into_bytes());

                // JSON с реальными числами
                let mut params_json = serde_json::Map::new();
                params_json.insert("apiKey".into(), Value::String(api_key.to_string()));
                params_json.insert("positionSide".into(), Value::String("BOTH".into()));
                params_json.insert("price".into(), Value::Number(serde_json::Number::from_f64(*price).unwrap()));
                params_json.insert("quantity".into(), Value::Number(serde_json::Number::from_f64(*qty).unwrap()));
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

                (id.to_string(), msg_json.to_string())
            }


            Command::CancelLimitOrder(symbol, order_id) => {
                let mut p: BTreeMap<&str, String> = BTreeMap::new();
                p.insert("apiKey", api_key);
                p.insert("orderId", order_id.clone());
                p.insert("recvWindow", "5000".to_string());
                p.insert("symbol", symbol.to_uppercase());
                p.insert("timestamp", chrono::Utc::now().timestamp_millis().to_string());

                let query = p.iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join("&");

                let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC key");
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
                (id, msg_json.to_string())
            }
        }
    }
   
   
   
   
    // ─────────────────────────── Public API ───────────────────────────

    /// Отправка произвольной команды с калбэком
    pub async fn send_command<F>(&self, cmd: Command, callback: F)
    where
        F: Fn(Value) + Send + Sync + 'static,
    {
        let id = self.next_id().await;
        let (msg_id, msg) = self.build_message_with_id(&cmd).await;

        {
            let mut pending = self.pending.lock().await;
            pending.insert(msg_id.clone(), Box::new(callback));
        }

        if let Err(e) = self.cmd_tx.send(cmd).await {
            tracing::error!("Send cmd channel error: {e}");
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
            Command::SendLimitOrder(symbol.into(), price.into(), size.into(), side.into()),
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