// src/exchange_data.rs

use tokio::sync::{mpsc, Mutex, broadcast};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use simd_json::serde as simd_serde;
use std::{sync::Arc, time::{Instant, SystemTime}};
use crate::ffi_types::{CEvent, CEventData, CBookTicker, CTrade};

// ═══════════════════════════════════════════════════════════
// УДАЛЯЕМ старый Event enum!
// Теперь используем только CEvent
// ═══════════════════════════════════════════════════════════

// Временные структуры только для десериализации JSON
#[derive(Debug, Deserialize)]
struct RawBookTicker {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "b")]
    bid_price: String,
    #[serde(rename = "a")]
    ask_price: String,
    #[serde(rename = "B")]
    bid_qty: String,
    #[serde(rename = "A")]
    ask_qty: String,
    #[serde(rename = "E")]
    time: i64,
}

#[derive(Debug, Deserialize)]
struct RawTrade {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    qty: String,
    #[serde(rename = "m")]
    is_maker: bool,
    #[serde(rename = "E")]
    time: i64,
}

#[derive(Debug)]
pub enum Command {
    SubscribeBookticker(String),
    UnsubscribeBookticker(String),
    SubscribeTrades(String),
    UnsubscribeTrades(String),
    ListSubscriptions,
}

pub struct ExchangeData {
    ws_url: String,
    is_connected: Arc<Mutex<bool>>,
    cmd_tx: mpsc::Sender<Command>,
    pub event_tx: broadcast::Sender<CEvent>,  // ← теперь CEvent!
    start_time: Instant,
}

impl ExchangeData {
    pub fn new(ws_url: String, event_tx: broadcast::Sender<CEvent>) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        
        let manager = Arc::new(Self {
            ws_url: ws_url.clone(),
            event_tx,
            is_connected: Arc::new(Mutex::new(false)),
            cmd_tx,
            start_time: Instant::now(),
        });
        
        let manager_clone = manager.clone();
        tokio::spawn(async move {
            manager_clone.run_socket(ws_url, cmd_rx).await;
        });
        
        manager
    }

    async fn run_socket(
        self: Arc<Self>,
        ws_url: String,
        mut cmd_rx: mpsc::Receiver<Command>
    ) {
        loop {
            tracing::info!("Trying to connect...");
            match connect_async(&ws_url).await {
                Ok((mut ws, _)) => {
                    tracing::info!("Connected to {ws_url}");
                    *self.is_connected.lock().await = true;
                    let (mut write, mut read) = ws.split();

                    let mgr = self.clone();
                    let cmd_tx = self.cmd_tx.clone();

                    let reader_mgr = mgr.clone();
                    let reader = tokio::spawn(async move {
                        loop {
                            match tokio::time::timeout(
                                tokio::time::Duration::from_secs(5),
                                read.next()
                            ).await {
                                Ok(Some(Ok(Message::Text(txt)))) => {
                                    reader_mgr.handle_text(txt).await;
                                }
                                Ok(Some(Ok(Message::Close(f)))) => {
                                    tracing::warn!("Server closed socket: {:?}", f);
                                    break;
                                }
                                Ok(Some(Err(e))) => {
                                    tracing::error!("Read error: {e}");
                                    break;
                                }
                                Ok(None) => {
                                    tracing::warn!("Stream ended");
                                    break;
                                }
                                Err(_) => {
                                    // Timeout - отправляем ping
                                    if cmd_tx.send(Command::ListSubscriptions).await.is_err() {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                    });

                    while let Some(cmd) = cmd_rx.recv().await {
                        if !*self.is_connected.lock().await {
                            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                            continue;
                        }
                        let msg = Self::command_to_json(cmd);
                        if write.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }

                    let _ = reader.await;
                    *self.is_connected.lock().await = false;
                    tracing::info!("Reconnecting in 3 sec...");
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                }
                Err(e) => {
                    *self.is_connected.lock().await = false;
                    tracing::error!("WS connect error: {e:?}");
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                }
            }
        }
    }

    async fn handle_text(&self, mut txt: String) {
        // для вывода в консоль
        // let start = tokio::time::Instant::now();
        let received_at_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        
        // ═══════════════════════════════════════════════════════════
        // СРАЗУ КОНВЕРТИРУЕМ В C-ТИПЫ!
        // ═══════════════════════════════════════════════════════════
        
        if txt.contains("\"bookTicker\"") {
            match unsafe { simd_serde::from_str::<RawBookTicker>(txt.as_mut_str()) } {
                Ok(bt) => {
                    // Конвертируем в C-тип
                    let mut symbol = [0u8; 16];
                    let bytes = bt.symbol.as_bytes();
                    let len = bytes.len().min(15);
                    symbol[..len].copy_from_slice(&bytes[..len]);
                    
                    let c_event = CEvent {
                        event_type: 0,
                        data: CEventData {
                            book_ticker: CBookTicker {
                                symbol,
                                symbol_len: len as u8,
                                bid_price: bt.bid_price.parse().unwrap_or(0.0),
                                ask_price: bt.ask_price.parse().unwrap_or(0.0),
                                bid_qty: bt.bid_qty.parse().unwrap_or(0.0),
                                ask_qty: bt.ask_qty.parse().unwrap_or(0.0),
                                time: bt.time,
                            }
                        },
                        received_at_ns,
                    };
                    
                    // let book = unsafe { &c_event.data.book_ticker };
                    // println!(
                    //     "📊 BOOK {:>10} bid={:<10.4} ask={:<10.4} Δ{:?}",
                    //     book.symbol_str(),
                    //     book.bid_price,
                    //     book.ask_price,
                    //     start.elapsed()
                    // );

                    // Отправляем C-тип
                    let _ = self.event_tx.send(c_event);
                }
                Err(e) => tracing::error!("BookTicker parse error: {e:?}"),
            }
        } else if txt.contains("\"trade\"") {
            match unsafe { simd_serde::from_str::<RawTrade>(txt.as_mut_str()) } {
                Ok(t) => {
                    let mut symbol = [0u8; 16];
                    let bytes = t.symbol.as_bytes();
                    let len = bytes.len().min(15);
                    symbol[..len].copy_from_slice(&bytes[..len]);
                    
                    let mut qty: f64 = t.qty.parse().unwrap_or(0.0);
                    if t.is_maker {
                        qty = -qty;
                    }
                    
                    let c_event = CEvent {
                        event_type: 1,
                        data: CEventData {
                            trade: CTrade {
                                symbol,
                                symbol_len: len as u8,
                                price: t.price.parse().unwrap_or(0.0),
                                qty,
                                time: t.time,
                            }
                        },
                        received_at_ns
                    };
                    
                    // let trade = unsafe { &c_event.data.trade };
                    // let side = if trade.qty > 0.0 { "BUY" } else { "SELL" };
                    // println!(
                    //     "💰 TRADE {:>10} price={:<10.4} qty={:<8.4} {} Δ{:?}",
                    //     trade.symbol_str(),
                    //     trade.price,
                    //     trade.qty.abs(),
                    //     side,
                    //     start.elapsed()
                    // );

                    let _ = self.event_tx.send(c_event);
                }
                Err(e) => tracing::error!("Trade parse error: {e:?}"),
            }
        }
    }

    fn command_to_json(cmd: Command) -> String {
        let msg = match cmd {
            Command::SubscribeBookticker(sym) => serde_json::json!({
                "method": "SUBSCRIBE",
                "params": [format!("{sym}@bookTicker")],
                "id": 1
            }),
            Command::UnsubscribeBookticker(sym) => serde_json::json!({
                "method": "UNSUBSCRIBE",
                "params": [format!("{sym}@bookTicker")],
                "id": 1
            }),
            Command::SubscribeTrades(sym) => serde_json::json!({
                "method": "SUBSCRIBE",
                "params": [format!("{sym}@trade")],
                "id": 1
            }),
            Command::UnsubscribeTrades(sym) => serde_json::json!({
                "method": "UNSUBSCRIBE",
                "params": [format!("{sym}@trade")],
                "id": 1
            }),
            Command::ListSubscriptions => serde_json::json!({
                "method": "LIST_SUBSCRIPTIONS",
                "id": 1
            }),
        };
        msg.to_string()
    }

    pub async fn subscribe_bookticker(&self, symbol: &str) -> anyhow::Result<()> {
        self.cmd_tx.send(Command::SubscribeBookticker(symbol.to_lowercase())).await?;
        Ok(())
    }

    pub async fn subscribe_trades(&self, symbol: &str) -> anyhow::Result<()> {
        self.cmd_tx.send(Command::SubscribeTrades(symbol.to_lowercase())).await?;
        Ok(())
    }

    pub async fn unsubscribe_bookticker(&self, symbol: &str) -> anyhow::Result<()> {
        self.cmd_tx.send(Command::UnsubscribeBookticker(symbol.to_lowercase())).await?;
        Ok(())
    }

    pub async fn unsubscribe_trades(&self, symbol: &str) -> anyhow::Result<()> {
        self.cmd_tx.send(Command::UnsubscribeTrades(symbol.to_lowercase())).await?;
        Ok(())
    }
}