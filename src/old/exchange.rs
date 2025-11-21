use tokio::{
    sync::{mpsc, Mutex},
    time::{sleep, Duration, Instant, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio::sync::broadcast;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Deserializer};
use serde::de;
use serde_json::json;
use simd_json::serde as simd_serde;
use std::sync::Arc;


#[derive(Debug, Clone)]
pub enum Event {
    BookTicker(BookTicker),
    Trade(Trade),
}

// ─────────────────────────── Команды ───────────────────────────
#[derive(Debug)]
pub enum Command {
    SubscribeBookticker(String),
    UnsubscribeBookticker(String),
    SubscribeTrades(String),
    UnsubscribeTrades(String),
    ListSubscriptions,
}

// ─────────────────────────── Менеджер ───────────────────────────
pub struct ExchangeData {
    ws_url: String,
    is_connected: Arc<Mutex<bool>>,
    cmd_tx: mpsc::Sender<Command>,
    pub event_tx: broadcast::Sender<Event>,
}

impl ExchangeData {
    pub fn new(ws_url: String, event_tx: broadcast::Sender<Event>) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let manager = Arc::new(Self {
            ws_url: ws_url.clone(),
            is_connected: Arc::new(Mutex::new(false)),
            cmd_tx,
            event_tx,
        });
        let manager_clone = manager.clone();
        tokio::spawn(async move {
            manager_clone.run_socket(ws_url, cmd_rx).await;
        });
        manager
    }

    async fn run_socket(self: Arc<Self>, ws_url: String, mut cmd_rx: mpsc::Receiver<Command>) {
        loop {
            tracing::info!("Trying to connect...");
            match connect_async(&ws_url).await {
                Ok((mut ws, _)) => {
                    tracing::info!("Connected to {ws_url}");
                    *self.is_connected.lock().await = true;
                    let (mut write, mut read) = ws.split();

                    // менеджер для таски‑читателя
                    let mgr = self.clone();
                    let cmd_tx = self.cmd_tx.clone();

                    // ───── Reader с проверкой активности ─────
                    let reader_mgr = mgr.clone();
                    let reader = tokio::spawn(async move {
                        let mut last_msg = Instant::now();
                        let mut waiting_list = false;
                        let mut list_request_time = Instant::now();

                        loop {
                            let next_msg = timeout(Duration::from_secs(5), read.next()).await;
                            match next_msg {
                                Ok(Some(Ok(Message::Text(txt)))) => {
                                    last_msg = Instant::now();
                                    waiting_list = false;
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
                                    // 5 сек тишины
                                    if !waiting_list {
                                        // tracing::warn!(
                                        //     "No messages for 5 s — sending LIST_SUBSCRIPTIONS"
                                        // );
                                        if cmd_tx.send(Command::ListSubscriptions).await.is_err() {
                                            tracing::warn!("command channel closed");
                                            *reader_mgr.is_connected.lock().await = false;
                                            break;
                                        }
                                        waiting_list = true;
                                        list_request_time = Instant::now();
                                    } else if list_request_time.elapsed() > Duration::from_secs(5) {
                                        // tracing::error!(
                                        //     "No LIST_SUBSCRIPTIONS answer for 5 s — reconnect"
                                        // );
                                        *reader_mgr.is_connected.lock().await = false;
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                    });

                    // ───── Writer: слушаем команды и шлём JSON в Binance ─────
                    while let Some(cmd) = cmd_rx.recv().await {
                        if !*self.is_connected.lock().await {
                            tracing::warn!("Not connected, skip command");
                            sleep(Duration::from_millis(200)).await;
                            continue;
                        }
                        let msg = Self::command_to_json(cmd);
                        if let Err(e) = write.send(Message::Text(msg)).await {
                            tracing::error!("Send failed: {e}");
                            *self.is_connected.lock().await = false;
                            break;
                        }
                    }

                    let _ = reader.await;
                    *self.is_connected.lock().await = false;
                    tracing::info!("Reconnecting in 3 sec...");
                    sleep(Duration::from_secs(3)).await;
                }
                Err(e) => {
                    *self.is_connected.lock().await = false;
                    tracing::error!("Data WS connect error: {e:?}, retrying");
                    sleep(Duration::from_secs(3)).await;
                }
            }
        }
    }

    async fn handle_text(&self, mut txt: String) {
        let start = Instant::now();

        if txt.contains("\"bookTicker\"") {
            match unsafe { simd_serde::from_str::<BookTicker>(txt.as_mut_str()) } {
                
                Ok(bt) => {
                    let _ = self.event_tx.send(Event::BookTicker(bt.clone()));
                    println!(
                        "BOOK {:>10} bid={:<10.4} ask={:<10.4} Δ{:?}",
                        bt.symbol, bt.bid_price, bt.ask_price, start.elapsed()
                    )
                }
                Err(e) => tracing::error!("BookTicker parse error: {e:?}"),
            }
        } else if txt.contains("\"trade\"") {
            match unsafe { simd_serde::from_str::<Trade>(txt.as_mut_str()) } {
                Ok(tr) => {
                    let _ = self.event_tx.send(Event::Trade(tr.clone()));
                    let side = if tr.qty > 0.0 { "BUY" } else { "SELL" };
                    println!(
                        "TRADE {:>10} price={:<10.4} qty={:<8.4} {} Δ{:?}",
                        tr.symbol,
                        tr.price,
                        tr.qty.abs(),
                        side,
                        start.elapsed()
                    );
                }
                Err(e) => tracing::error!("Trade parse error: {e:?}"),
            }
        } else {
            if let Err(e) = Self::handle_system_message(txt.as_str()) {
                tracing::warn!("System msg parse failed: {e:?}");
            }
        }
    }

    fn command_to_json(cmd: Command) -> String {
        let msg = match cmd {
            Command::SubscribeBookticker(sym) => json!({
                "method": "SUBSCRIBE",
                "params": [format!("{sym}@bookTicker")],
                "id": 1
            }),
            Command::UnsubscribeBookticker(sym) => json!({
                "method": "UNSUBSCRIBE",
                "params": [format!("{sym}@bookTicker")],
                "id": 1
            }),
            Command::SubscribeTrades(sym) => json!({
                "method": "SUBSCRIBE",
                "params": [format!("{sym}@trade")],
                "id": 1
            }),
            Command::UnsubscribeTrades(sym) => json!({
                "method": "UNSUBSCRIBE",
                "params": [format!("{sym}@trade")],
                "id": 1
            }),
            Command::ListSubscriptions => json!({
                "method": "LIST_SUBSCRIPTIONS",
                "id": 1
            }),
        };
        msg.to_string()
    }

    /// Быстрая обработка системных сообщений (ответы, ошибки)
    fn handle_system_message(raw: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut tmp = raw.to_string();
        let v: serde_json::Value = unsafe { simd_serde::from_str(&mut tmp)? };
        if let Some(result) = v.get("result") {
            tracing::info!("WS result: {result}");
        } else if let Some(code) = v.get("code") {
            tracing::error!("WS error code {code}: {:?}", v.get("msg"));
        } else {
            tracing::debug!("WS system msg: {}", v.to_string());
        }
        Ok(())
    }

    pub async fn list_subscriptions(&self) -> anyhow::Result<()> {
        tracing::info!("Sending listSubscriptions command");
        self.cmd_tx
            .send(Command::ListSubscriptions)
            .await?;
        Ok(())
    }
    
    pub async fn subscribe_bookticker(&self, symbol: &str) -> anyhow::Result<()> {
        self.cmd_tx
            .send(Command::SubscribeBookticker(symbol.to_lowercase()))
            .await?;
        Ok(())
    }

    pub async fn subscribe_trades(&self, symbol: &str) -> anyhow::Result<()> {
        self.cmd_tx
            .send(Command::SubscribeTrades(symbol.to_lowercase()))
            .await?;
        Ok(())
    }

    pub async fn unsubscribe_bookticker(&self, symbol: &str) -> anyhow::Result<()> {
        self.cmd_tx
            .send(Command::UnsubscribeBookticker(symbol.to_lowercase()))
            .await?;
        Ok(())
    }

    pub async fn unsubscribe_trades(&self, symbol: &str) -> anyhow::Result<()> {
        self.cmd_tx
            .send(Command::UnsubscribeTrades(symbol.to_lowercase()))
            .await?;
        Ok(())
    }
}

// ─────────────────────────── Типы данных ───────────────────────────
#[derive(Debug, Deserialize, Clone)]
pub struct BookTicker {
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "b", deserialize_with = "de_str_to_f64")]
    pub bid_price: f64,
    #[serde(rename = "a", deserialize_with = "de_str_to_f64")]
    pub ask_price: f64,
    #[serde(rename = "B", deserialize_with = "de_str_to_f64")]
    pub bid_qty: f64,
    #[serde(rename = "A", deserialize_with = "de_str_to_f64")]
    pub ask_qty: f64,
    #[serde(rename = "E")]
    pub time: i64,
}

impl BookTicker {
    /// Тип события для унификации
    pub const event: &'static str = "bookTicker";
}

fn de_str_to_f64<'de, D>(de: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(de)?;
    s.parse::<f64>().map_err(serde::de::Error::custom)
}

/// Событие "trade" c уже вычисленным знаком размера
#[derive(Debug, Clone)]
pub struct Trade {
    pub time: i64,
    pub symbol: String,
    pub price: f64,
    pub qty: f64,
}

impl Trade {
    /// Тип события для унификации
    pub const event: &'static str = "trade";
}


impl<'de> Deserialize<'de> for Trade {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTrade {
            #[serde(rename = "E")] E: i64,
            #[serde(rename = "s")] s: String,
            #[serde(rename = "p")] p: String,
            #[serde(rename = "q")] q: String,
            #[serde(rename = "m")] m: bool,
        }
        let raw = RawTrade::deserialize(deserializer)?;
        let price = raw.p.parse::<f64>().map_err(de::Error::custom)?;
        let mut qty = raw.q.parse::<f64>().map_err(de::Error::custom)?;
        if raw.m { qty = -qty; }
        Ok(Self { time: raw.E, symbol: raw.s, price, qty })
    }
}