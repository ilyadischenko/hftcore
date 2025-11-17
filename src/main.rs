use axum::{
    routing::post,
    extract::{Json, State},
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, Duration};
use std::sync::Arc;
use crate::exchange_data::{ExchangeData, Event};
use crate::exchange_trade::ExchangeTrade;

use tokio::sync::Mutex;
use serde_json::Value;

mod exchange_data;
mod exchange_trade;
// mod strategy_storage;      // ← НОВОЕ
mod routes;       // ← НОВОЕ

use strategy_storage::StrategyStorage;
use routes::strategy;

#[derive(Deserialize)]
struct TickerRequest {
    ticker: String,
}

#[derive(Clone)]
struct AppContext {
    data_manager: Arc<ExchangeData>,
    trade_manager: Arc<ExchangeTrade>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    // ---- Менеджеры данных и торговли ----------------
    let data_manager = ExchangeData::new("wss://fstream.binance.com/ws".to_string());

    let trade_manager = ExchangeTrade::new(
        "wss://ws-fapi.binance.com/ws-fapi/v1".to_string(),
        "ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm".to_string(),
        "3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4".to_string(),
    );

    let app_state = Arc::new(AppContext { data_manager, trade_manager });

    // ---- Хранилище стратегий ----------------
    let strategy_storage = Arc::new(
        StrategyStorage::new("./strategies").expect("Failed to create strategy storage")
    );

    // ---- Маршруты приложения -------------------------
    let app = Router::new()
        // Существующие роуты для данных
        .route("/subscribe/bookticker", post(subscribe_bookticker))
        .route("/unsubscribe/bookticker", post(unsubscribe_bookticker))
        .route("/subscribe/trades", post(subscribe_trades))
        .route("/unsubscribe/trades", post(unsubscribe_trades))
        .route("/latency/bookticker", post(measure_bookticker_latency))
        .route("/login", post(login_session))
        .with_state(app_state.clone())
        
        // ← НОВЫЕ РОУТЫ ДЛЯ СТРАТЕГИЙ
        .merge(strategy_routes(strategy_storage));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    tracing::info!("Server running on http://0.0.0.0:8080");
    axum::serve(listener, app).await.unwrap();
}

// ──────────────────────────── ROUTES (твои существующие) ────────────────────────────

async fn login_session(State(app): State<Arc<AppContext>>) {
    let order_id_holder = Arc::new(Mutex::new(None::<i64>));
    let order_id_ref = Arc::clone(&order_id_holder);

    app.trade_manager
        .send_limit_order("SOLUSDT", 190.10, 0.1, "BUY", move |resp: Value| {
            if let Some(order_id) = resp["result"]["orderId"].as_i64() {
                println!("✅ Ордер создан: {order_id}");
                let id_store = Arc::clone(&order_id_ref);
                tokio::spawn(async move {
                    *id_store.lock().await = Some(order_id);
                });
            } else {
                println!("❌ Не удалось получить orderId: {resp}");
            }
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let maybe_id = order_id_holder.lock().await.clone();
    if let Some(order_id) = maybe_id {
        tracing::info!("Можно отменить ордер с id = {order_id}");

        app.trade_manager
            .cancel_limit_order("SOLUSDT", &order_id.to_string(), |resp| {
                println!("🧹 Ответ на отмену: {resp}");
            })
            .await;
    } else {
        tracing::info!("⚠️ Не получили orderId — возможно, ордер ещё не подтвержден.");
    }

    tracing::info!("auth");
}

async fn subscribe_bookticker(
    State(app): State<Arc<AppContext>>,
    Json(req): Json<TickerRequest>,
) -> String {
    match app.data_manager.subscribe_bookticker(&req.ticker).await {
        Ok(_) => format!("Subscribed to {}", req.ticker),
        Err(e) => format!("Error: {e}"),
    }
}

async fn unsubscribe_bookticker(
    State(app): State<Arc<AppContext>>,
    Json(req): Json<TickerRequest>,
) -> String {
    match app.data_manager.unsubscribe_bookticker(&req.ticker).await {
        Ok(_) => format!("Unsubscribed from {}", req.ticker),
        Err(e) => format!("Error: {e}"),
    }
}

async fn subscribe_trades(
    State(app): State<Arc<AppContext>>,
    Json(req): Json<TickerRequest>,
) -> String {
    match app.data_manager.subscribe_trades(&req.ticker).await {
        Ok(_) => format!("Subscribed to {}", req.ticker),
        Err(e) => format!("Error: {e}"),
    }
}

async fn unsubscribe_trades(
    State(app): State<Arc<AppContext>>,
    Json(req): Json<TickerRequest>,
) -> String {
    match app.data_manager.unsubscribe_trades(&req.ticker).await {
        Ok(_) => format!("Unsubscribed from {}", req.ticker),
        Err(e) => format!("Error: {e}"),
    }
}

#[derive(Serialize)]
struct LatencyStats {
    ticker: String,
    avg_latency_ms: f64,
    min_latency_ms: f64,
    max_latency_ms: f64,
}

async fn measure_bookticker_latency(
    State(app): State<Arc<AppContext>>,
    Json(req): Json<TickerRequest>,
) -> Json<LatencyStats> {
    let data_manager = &app.data_manager;

    if let Err(e) = data_manager.subscribe_bookticker(&req.ticker).await {
        tracing::error!("subscribe error: {e}");
    }

    let mut rx = data_manager.event_tx.subscribe();
    let start = Instant::now();
    let mut latencies = Vec::new();

    while start.elapsed() < Duration::from_secs(10) {
        if let Ok(Event::BookTicker(bt)) = rx.recv().await {
            if bt.symbol.eq_ignore_ascii_case(&req.ticker) {
                let now = chrono::Utc::now().timestamp_millis();
                latencies.push((now - bt.time) as f64);
            }
        }
    }

    if let Err(e) = data_manager.unsubscribe_bookticker(&req.ticker).await {
        tracing::warn!("unsubscribe error: {e}");
    }

    let stats = if latencies.is_empty() {
        LatencyStats {
            ticker: req.ticker,
            avg_latency_ms: 0.0,
            min_latency_ms: 0.0,
            max_latency_ms: 0.0,
        }
    } else {
        let sum: f64 = latencies.iter().sum();
        let avg = sum / latencies.len() as f64;
        let min = latencies.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = latencies.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        LatencyStats {
            ticker: req.ticker,
            avg_latency_ms: avg,
            min_latency_ms: min,
            max_latency_ms: max,
        }
    };

    Json(stats)
}