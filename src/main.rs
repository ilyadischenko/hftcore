// src/main.rs

use axum::{
    routing::{post, get},
    extract::{Json, State},
    Router,
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use tokio::{sync::broadcast, time::{Duration, Instant}};
use std::sync::Arc;
use tokio::sync::Mutex;
use serde_json::Value;

mod ffi_types;
mod exchange_data;
mod exchange_trade;
mod routes;
mod strategies;
mod user_data;

use crate::exchange_data::ExchangeData;
use crate::exchange_trade::ExchangeTrade;
use crate::routes::strategy::{self, AppState};
use crate::routes::user_data::{routes as user_data_routes, UserDataState};
use crate::strategies::{StrategyStorage, StrategyRunner};
use crate::strategies::init_trading;
use crate::user_data::UserDataManager;
use crate::ffi_types::CEvent;

// ═══════════════════════════════════════════════════════════
// REQUEST/RESPONSE ТИПЫ
// ═══════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct TickerRequest {
    ticker: String,
}

#[derive(Deserialize)]
struct TestOrderRequest {
    api_key: String,
    secret_key: String,
    symbol: String,
    price: f64,
    quantity: f64,
    side: String,
}

#[derive(Deserialize)]
struct CancelOrderRequest {
    api_key: String,
    secret_key: String,
    symbol: String,
    order_id: String,
}

#[derive(Serialize)]
struct OrderResponse {
    success: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    order_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Serialize)]
struct OrderPingResponse {
    success: bool,
    ping_ms: f64,
    order_place_ms: f64,
    order_cancel_ms: f64,
    total_ms: f64,
    order_id: Option<i64>,
    test_price: f64,
    current_bid: f64,
    message: String,
}

/// Контекст для data/trade роутов
#[derive(Clone)]
struct DataContext {
    data_manager: Arc<ExchangeData>,
    trade_manager: Arc<ExchangeTrade>,
    event_broadcaster: broadcast::Sender<CEvent>,
}

// ═══════════════════════════════════════════════════════════
// MAIN
// ═══════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let (event_tx, _) = broadcast::channel::<CEvent>(10000);

    // ═══════════════════════════════════════════════════════════
    // DATA MANAGER
    // ═══════════════════════════════════════════════════════════
    
    let data_manager = ExchangeData::new(
        "wss://fstream.binance.com/ws".to_string(), 
        event_tx.clone()
    );

    // ═══════════════════════════════════════════════════════════
    // TRADE MANAGER
    // ═══════════════════════════════════════════════════════════
    
    let trade_manager = ExchangeTrade::new(
        "wss://ws-fapi.binance.com/ws-fapi/v1".to_string()
    );
    
    match trade_manager.sync_time().await {
        Ok(offset) => {
            if offset.abs() > 1000 {
                tracing::warn!("⚠️ Large time offset: {}ms", offset);
            } else {
                tracing::info!("✅ Time offset: {}ms", offset);
            }
        }
        Err(e) => {
            tracing::error!("❌ Time sync failed: {}", e);
            trade_manager.set_time_offset(-1000);
        }
    }

    init_trading(trade_manager.clone());

    // ═══════════════════════════════════════════════════════════
    // USER DATA MANAGER
    // ═══════════════════════════════════════════════════════════

    let user_data_manager = UserDataManager::new();


    // ═══════════════════════════════════════════════════════════
    // STRATEGY STORAGE & RUNNER
    // ═══════════════════════════════════════════════════════════
    
    let storage = Arc::new(
        StrategyStorage::new("./strategies/db")
            .expect("Failed to create strategy storage")
    );
    
    // let runner = StrategyRunner::new();
    let runner = StrategyRunner::new(user_data_manager.clone());  // ← Передаём manager

    // ═══════════════════════════════════════════════════════════
    // STATES
    // ═══════════════════════════════════════════════════════════
    
    let data_state = Arc::new(DataContext { 
        data_manager, 
        trade_manager, 
        event_broadcaster: event_tx.clone(),
    });
    
    let strategy_state = AppState {
        storage,
        runner,
        event_tx,
    };

    let user_data_state = UserDataState {
        manager: user_data_manager,
    };

    // ═══════════════════════════════════════════════════════════
    // ROUTES
    // ═══════════════════════════════════════════════════════════
    
    let data_routes = Router::new()
        .route("/subscribe/bookticker", post(subscribe_bookticker))
        .route("/unsubscribe/bookticker", post(unsubscribe_bookticker))
        .route("/subscribe/trades", post(subscribe_trades))
        .route("/unsubscribe/trades", post(unsubscribe_trades))
        .route("/order/test", post(test_order))
        .route("/order/cancel", post(cancel_order))
        .route("/ping/order", get(ping_order))
        // .route("/login", post(login_session))
        .with_state(data_state);
    
    let app = Router::new()
        .merge(data_routes)
        .nest("/api", strategy::routes(strategy_state))
        .nest("/api", user_data_routes(user_data_state));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    tracing::info!("🚀 Server running on http://0.0.0.0:8080");
    tracing::info!("📚 Strategy API at /api/strategies");
    tracing::info!("📊 Instances API at /api/instances");
    tracing::info!("📡 User Data API at /api/userdata/streams");
    axum::serve(listener, app).await.unwrap();
}

// ═══════════════════════════════════════════════════════════
// ORDER HANDLERS
// ═══════════════════════════════════════════════════════════

async fn test_order(
    State(app): State<Arc<DataContext>>,
    Json(req): Json<TestOrderRequest>,
) -> (StatusCode, Json<OrderResponse>) {
    tracing::info!("📝 Test order: {} {} {} @ {}", req.side, req.quantity, req.symbol, req.price);

    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Arc::new(Mutex::new(Some(tx)));

    app.trade_manager
        .send_market_order(
            &req.api_key,
            &req.secret_key,
            &req.symbol,
            req.quantity,
            &req.side,
            move |resp: Value| {
                let tx_clone = tx.clone();
                tokio::spawn(async move {
                    if let Some(sender) = tx_clone.lock().await.take() {
                        let _ = sender.send(resp);
                    }
                });
            },
        )
        .await;

    match tokio::time::timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(response)) => {
            if let Some(error) = response.get("error") {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OrderResponse {
                        success: false,
                        message: format!("Order failed: {}", error),
                        order_id: None,
                        data: Some(response),
                    }),
                );
            }

            let order_id = response["result"]["orderId"].as_i64();
            (
                StatusCode::OK,
                Json(OrderResponse {
                    success: true,
                    message: "Order created".to_string(),
                    order_id,
                    data: Some(response),
                }),
            )
        }
        Ok(Err(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OrderResponse {
                success: false,
                message: "Channel closed".to_string(),
                order_id: None,
                data: None,
            }),
        ),
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            Json(OrderResponse {
                success: false,
                message: "Timeout (10s)".to_string(),
                order_id: None,
                data: None,
            }),
        ),
    }
}

async fn cancel_order(
    State(app): State<Arc<DataContext>>,
    Json(req): Json<CancelOrderRequest>,
) -> (StatusCode, Json<OrderResponse>) {
    tracing::info!("🗑️ Cancel order: {} {}", req.symbol, req.order_id);

    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Arc::new(Mutex::new(Some(tx)));

    app.trade_manager
        .cancel_limit_order(
            &req.api_key,
            &req.secret_key,
            &req.symbol,
            &req.order_id,
            move |resp: Value| {
                let tx_clone = tx.clone();
                tokio::spawn(async move {
                    if let Some(sender) = tx_clone.lock().await.take() {
                        let _ = sender.send(resp);
                    }
                });
            },
        )
        .await;

    match tokio::time::timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(response)) => {
            if let Some(error) = response.get("error") {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OrderResponse {
                        success: false,
                        message: format!("Cancel failed: {}", error),
                        order_id: None,
                        data: Some(response),
                    }),
                );
            }

            (
                StatusCode::OK,
                Json(OrderResponse {
                    success: true,
                    message: "Order cancelled".to_string(),
                    order_id: None,
                    data: Some(response),
                }),
            )
        }
        Ok(Err(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OrderResponse {
                success: false,
                message: "Internal error".to_string(),
                order_id: None,
                data: None,
            }),
        ),
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            Json(OrderResponse {
                success: false,
                message: "Timeout".to_string(),
                order_id: None,
                data: None,
            }),
        ),
    }
}

// ═══════════════════════════════════════════════════════════
// DATA ROUTES
// ═══════════════════════════════════════════════════════════

async fn subscribe_bookticker(
    State(app): State<Arc<DataContext>>,
    Json(req): Json<TickerRequest>,
) -> String {
    match app.data_manager.subscribe_bookticker(&req.ticker).await {
        Ok(_) => format!("Subscribed to {}", req.ticker),
        Err(e) => format!("Error: {e}"),
    }
}

async fn unsubscribe_bookticker(
    State(app): State<Arc<DataContext>>,
    Json(req): Json<TickerRequest>,
) -> String {
    match app.data_manager.unsubscribe_bookticker(&req.ticker).await {
        Ok(_) => format!("Unsubscribed from {}", req.ticker),
        Err(e) => format!("Error: {e}"),
    }
}

async fn subscribe_trades(
    State(app): State<Arc<DataContext>>,
    Json(req): Json<TickerRequest>,
) -> String {
    match app.data_manager.subscribe_trades(&req.ticker).await {
        Ok(_) => format!("Subscribed to {}", req.ticker),
        Err(e) => format!("Error: {e}"),
    }
}

async fn unsubscribe_trades(
    State(app): State<Arc<DataContext>>,
    Json(req): Json<TickerRequest>,
) -> String {
    match app.data_manager.unsubscribe_trades(&req.ticker).await {
        Ok(_) => format!("Unsubscribed from {}", req.ticker),
        Err(e) => format!("Error: {e}"),
    }
}

// ═══════════════════════════════════════════════════════════
// LEGACY
// ═══════════════════════════════════════════════════════════

// async fn login_session(State(app): State<Arc<DataContext>>) {
//     let order_id_holder = Arc::new(Mutex::new(None::<i64>));
//     let order_id_ref = Arc::clone(&order_id_holder);

//     app.trade_manager
//         .send_limit_order(
//             "YOUR_API_KEY",
//             "YOUR_SECRET",
//             "SOLUSDT",
//             190.10,
//             0.1,
//             "BUY",
//             move |resp: Value| {
//                 if let Some(order_id) = resp["result"]["orderId"].as_i64() {
//                     let id_store = Arc::clone(&order_id_ref);
//                     tokio::spawn(async move {
//                         *id_store.lock().await = Some(order_id);
//                     });
//                 }
//             },
//         )
//         .await;

//     tokio::time::sleep(Duration::from_secs(1)).await;

//     if let Some(order_id) = order_id_holder.lock().await.clone() {
//         app.trade_manager
//             .cancel_limit_order(
//                 "YOUR_API_KEY",
//                 "YOUR_SECRET",
//                 "SOLUSDT",
//                 &order_id.to_string(),
//                 |resp| println!("Cancel: {resp}"),
//             )
//             .await;
//     }
// }


// ═══════════════════════════════════════════════════════════
// PING ORDER
// ═══════════════════════════════════════════════════════════

async fn ping_order(
    State(app): State<Arc<DataContext>>,
) -> (StatusCode, Json<OrderPingResponse>) {
    tracing::info!("🏓 Starting order ping test...");
    
    // 1. Получаем текущую цену
    let mut event_rx = app.event_broadcaster.subscribe();
    
    let current_bid = loop {
        match tokio::time::timeout(Duration::from_secs(5), event_rx.recv()).await {
            Ok(Ok(event)) if event.event_type == 0 => {
                let bt = unsafe { &event.data.book_ticker };
                let symbol = unsafe {
                    std::str::from_utf8_unchecked(&bt.symbol[..bt.symbol_len as usize])
                };
                
                if symbol.eq_ignore_ascii_case("solusdt") {
                    break bt.bid_price;
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(OrderPingResponse {
                    success: false,
                    ping_ms: 0.0, order_place_ms: 0.0, order_cancel_ms: 0.0, total_ms: 0.0,
                    order_id: None, test_price: 0.0, current_bid: 0.0,
                    message: "Event channel closed".to_string(),
                }));
            }
            Err(_) => {
                return (StatusCode::REQUEST_TIMEOUT, Json(OrderPingResponse {
                    success: false,
                    ping_ms: 0.0, order_place_ms: 0.0, order_cancel_ms: 0.0, total_ms: 0.0,
                    order_id: None, test_price: 0.0, current_bid: 0.0,
                    message: "No SOLUSDT data. Subscribe first.".to_string(),
                }));
            }
        }
    };
    
    // 2. Безопасная цена (50% от bid)
    let safe_price = (current_bid * 0.5 * 100.0).round() / 100.0;
    tracing::info!("Bid: ${:.2}, Test price: ${:.2}", current_bid, safe_price);
    
    // 3. Выставляем ордер
    let (place_tx, place_rx) = tokio::sync::oneshot::channel();
    let place_tx = Arc::new(Mutex::new(Some(place_tx)));
    let start_place = Instant::now();
    
    app.trade_manager
        .send_limit_order(
            "YOUR_API_KEY",
            "YOUR_SECRET",
            "SOLUSDT",
            safe_price,
            0.1,
            "BUY",
            move |resp: Value| {
                let tx = place_tx.clone();
                tokio::spawn(async move {
                    if let Some(sender) = tx.lock().await.take() {
                        let _ = sender.send(resp);
                    }
                });
            },
        )
        .await;
    
    let (order_id, order_place_ms) = match tokio::time::timeout(Duration::from_secs(10), place_rx).await {
        Ok(Ok(response)) => {
            let elapsed = start_place.elapsed().as_secs_f64() * 1000.0;
            
            if response.get("error").is_some() {
                return (StatusCode::BAD_REQUEST, Json(OrderPingResponse {
                    success: false, ping_ms: elapsed, order_place_ms: elapsed,
                    order_cancel_ms: 0.0, total_ms: elapsed, order_id: None,
                    test_price: safe_price, current_bid,
                    message: format!("Order failed: {}", response),
                }));
            }
            
            match response["result"]["orderId"].as_i64() {
                Some(id) => (id, elapsed),
                None => return (StatusCode::INTERNAL_SERVER_ERROR, Json(OrderPingResponse {
                    success: false, ping_ms: elapsed, order_place_ms: elapsed,
                    order_cancel_ms: 0.0, total_ms: elapsed, order_id: None,
                    test_price: safe_price, current_bid,
                    message: "No orderId".to_string(),
                })),
            }
        }
        _ => return (StatusCode::REQUEST_TIMEOUT, Json(OrderPingResponse {
            success: false, ping_ms: 10000.0, order_place_ms: 10000.0,
            order_cancel_ms: 0.0, total_ms: 10000.0, order_id: None,
            test_price: safe_price, current_bid,
            message: "Timeout".to_string(),
        })),
    };
    
    tracing::info!("✅ Order placed in {:.2}ms | ID: {}", order_place_ms, order_id);
    
    // 4. Отменяем ордер
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let cancel_tx = Arc::new(Mutex::new(Some(cancel_tx)));
    let start_cancel = Instant::now();
    
    app.trade_manager
        .cancel_limit_order(
            "YOUR_API_KEY",
            "YOUR_SECRET",
            "SOLUSDT",
            &order_id.to_string(),
            move |resp: Value| {
                let tx = cancel_tx.clone();
                tokio::spawn(async move {
                    if let Some(sender) = tx.lock().await.take() {
                        let _ = sender.send(resp);
                    }
                });
            },
        )
        .await;
    
    let order_cancel_ms = match tokio::time::timeout(Duration::from_secs(10), cancel_rx).await {
        Ok(Ok(_)) => start_cancel.elapsed().as_secs_f64() * 1000.0,
        _ => 10000.0,
    };
    
    tracing::info!("✅ Cancelled in {:.2}ms", order_cancel_ms);
    
    // 5. Результат
    let total_ms = order_place_ms + order_cancel_ms;
    
    (StatusCode::OK, Json(OrderPingResponse {
        success: true,
        ping_ms: order_place_ms,
        order_place_ms,
        order_cancel_ms,
        total_ms,
        order_id: Some(order_id),
        test_price: safe_price,
        current_bid,
        message: format!("Latency: {:.2}ms", order_place_ms),
    }))
}