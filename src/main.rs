use axum::{
    routing::{post, get},
    extract::{Json, State},
    Router,
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use tokio::{sync::broadcast, time::{Duration, Instant}};
use std::sync::Arc;
use crate::exchange_data::{ExchangeData};
use crate::exchange_trade::ExchangeTrade;

use tokio::sync::Mutex;
use serde_json::Value;

mod ffi_types;

mod exchange_data;
mod exchange_trade;

mod routes;
mod strategies;

use crate::routes::strategy;
use crate::strategies::{StrategyStorage, StrategyRunner};
use crate::ffi_types::{CEvent};
use crate::strategies::init_trading;

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
    side: String,  // "BUY" or "SELL"
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
struct LatencyStats {
    ticker: String,
    avg_latency_ms: f64,
    min_latency_ms: f64,
    max_latency_ms: f64,
}

#[derive(Clone)]
struct AppContext {
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

    let data_manager = ExchangeData::new(
        "wss://fstream.binance.com/ws".to_string(), 
        event_tx.clone()
    );

    let trade_manager = ExchangeTrade::new(
        "wss://ws-fapi.binance.com/ws-fapi/v1".to_string()
    );
    match trade_manager.sync_time().await {
        Ok(offset) => {
            if offset.abs() > 1000 {
                tracing::warn!(
                    "⚠️ Large time offset detected: {}ms. Consider syncing system time with NTP",
                    offset
                );
            } else {
                tracing::info!("✅ Time offset: {}ms (acceptable)", offset);
            }
        }
        Err(e) => {
            tracing::error!("❌ Time sync failed: {}. Orders may fail!", e);
            tracing::warn!("Using fallback offset: -1000ms");
            trade_manager.set_time_offset(-1000);
        }
    }

    init_trading(trade_manager.clone());

    let app_state = Arc::new(AppContext { 
        data_manager, 
        trade_manager, 
        event_broadcaster: event_tx.clone() 
    });

    let strategy_storage = Arc::new(
        StrategyStorage::new("./strategies/db")
            .expect("Failed to create strategy storage")
    );

    let strategy_runner = StrategyRunner::new();

    // ═══════════════════════════════════════════════════════════
    // ROUTES
    // ═══════════════════════════════════════════════════════════
    
    let app = Router::new()
        // Data routes
        .route("/subscribe/bookticker", post(subscribe_bookticker))
        .route("/unsubscribe/bookticker", post(unsubscribe_bookticker))
        .route("/subscribe/trades", post(subscribe_trades))
        .route("/unsubscribe/trades", post(unsubscribe_trades))
        
        // ═══════════════════════════════════════════════════════════
        // НОВЫЕ РОУТЫ ДЛЯ ОРДЕРОВ
        // ═══════════════════════════════════════════════════════════
        .route("/order/test", post(test_order))
        .route("/order/cancel", post(cancel_order))
        .route("/ping/order", get(ping_order))
        
        // Legacy
        .route("/login", post(login_session))
        
        .with_state(app_state.clone())
        
        // Strategy routes
        .merge(strategy::strategy_routes(strategy_storage.clone()))
        .merge(strategy::runtime_routes(strategy_storage, strategy_runner, event_tx));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    tracing::info!("🚀 Server running on http://0.0.0.0:8080");
    axum::serve(listener, app).await.unwrap();
}

// ═══════════════════════════════════════════════════════════
// ORDER HANDLERS
// ═══════════════════════════════════════════════════════════
/// POST /order/test - отправить тестовый ордер
async fn test_order(
    State(app): State<Arc<AppContext>>,
    Json(req): Json<TestOrderRequest>,
) -> (StatusCode, Json<OrderResponse>) {
    tracing::info!(
        "📝 Test order: {} {} {} @ {}",
        req.side,
        req.quantity,
        req.symbol,
        req.price
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    
    // ═══════════════════════════════════════════════════════════
    // ИСПРАВЛЕНИЕ: Оборачиваем tx в Mutex<Option<>>
    // ═══════════════════════════════════════════════════════════
    let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));

    app.trade_manager
        .send_market_order(
            &req.api_key,
            &req.secret_key,
            &req.symbol,
            // req.price,
            req.quantity,
            &req.side,
            move |resp: Value| {
                // Извлекаем tx из Option (можно только один раз)
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
                tracing::error!("❌ Order error: {}", error);
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

            if let Some(order_id) = response["result"]["orderId"].as_i64() {
                tracing::info!("✅ Order created: {}", order_id);
                (
                    StatusCode::OK,
                    Json(OrderResponse {
                        success: true,
                        message: "Order created successfully".to_string(),
                        order_id: Some(order_id),
                        data: Some(response),
                    }),
                )
            } else {
                tracing::warn!("⚠️ No orderId in response: {}", response);
                (
                    StatusCode::OK,
                    Json(OrderResponse {
                        success: true,
                        message: "Order sent but no orderId received".to_string(),
                        order_id: None,
                        data: Some(response),
                    }),
                )
            }
        }
        Ok(Err(_)) => {
            tracing::error!("❌ Response channel closed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OrderResponse {
                    success: false,
                    message: "Internal error: response channel closed".to_string(),
                    order_id: None,
                    data: None,
                }),
            )
        }
        Err(_) => {
            tracing::error!("⏰ Order timeout");
            (
                StatusCode::REQUEST_TIMEOUT,
                Json(OrderResponse {
                    success: false,
                    message: "Order request timeout (10s)".to_string(),
                    order_id: None,
                    data: None,
                }),
            )
        }
    }
}

/// POST /order/cancel - отменить ордер
async fn cancel_order(
    State(app): State<Arc<AppContext>>,
    Json(req): Json<CancelOrderRequest>,
) -> (StatusCode, Json<OrderResponse>) {
    tracing::info!("🗑️ Cancel order: {} {}", req.symbol, req.order_id);

    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));

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
                tracing::error!("❌ Cancel error: {}", error);
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

            tracing::info!("✅ Order cancelled");
            (
                StatusCode::OK,
                Json(OrderResponse {
                    success: true,
                    message: "Order cancelled successfully".to_string(),
                    order_id: None,
                    data: Some(response),
                }),
            )
        }
        Ok(Err(_)) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OrderResponse {
                    success: false,
                    message: "Internal error".to_string(),
                    order_id: None,
                    data: None,
                }),
            )
        }
        Err(_) => {
            (
                StatusCode::REQUEST_TIMEOUT,
                Json(OrderResponse {
                    success: false,
                    message: "Cancel request timeout".to_string(),
                    order_id: None,
                    data: None,
                }),
            )
        }
    }
}
// ═══════════════════════════════════════════════════════════
// LEGACY ROUTE
// ═══════════════════════════════════════════════════════════

async fn login_session(State(app): State<Arc<AppContext>>) {
    let order_id_holder = Arc::new(Mutex::new(None::<i64>));
    let order_id_ref = Arc::clone(&order_id_holder);

    app.trade_manager
        .send_limit_order(
            "ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm",
            "3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4",
            "SOLUSDT",
            190.10,
            0.1,
            "BUY",
            move |resp: Value| {
                if let Some(order_id) = resp["result"]["orderId"].as_i64() {
                    println!("✅ Ордер создан: {order_id}");
                    let id_store = Arc::clone(&order_id_ref);
                    tokio::spawn(async move {
                        *id_store.lock().await = Some(order_id);
                    });
                } else {
                    println!("❌ Не удалось получить orderId: {resp}");
                }
            },
        )
        .await;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let maybe_id = order_id_holder.lock().await.clone();
    if let Some(order_id) = maybe_id {
        tracing::info!("Можно отменить ордер с id = {order_id}");

        app.trade_manager
            .cancel_limit_order(
                "ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm",
                "3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4",
                "SOLUSDT",
                &order_id.to_string(),
                |resp| {
                    println!("🧹 Ответ на отмену: {resp}");
                },
            )
            .await;
    }
}

// ═══════════════════════════════════════════════════════════
// DATA ROUTES
// ═══════════════════════════════════════════════════════════

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


// В секции REQUEST/RESPONSE ТИПЫ добавь:

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




// В конце файла добавь handler:

// ═══════════════════════════════════════════════════════════
// PING ORDER - замер латентности выставления/отмены ордера
// ═══════════════════════════════════════════════════════════

/// GET /ping/order - проверка пинга выставления ордера
async fn ping_order(
    State(app): State<Arc<AppContext>>,
) -> (StatusCode, Json<OrderPingResponse>) {
    tracing::info!("🏓 Starting order ping test...");
    
    // ═══════════════════════════════════════════════════════════
    // 1. Получаем текущую цену из последнего события
    // ═══════════════════════════════════════════════════════════
    
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
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OrderPingResponse {
                        success: false,
                        ping_ms: 0.0,
                        order_place_ms: 0.0,
                        order_cancel_ms: 0.0,
                        total_ms: 0.0,
                        order_id: None,
                        test_price: 0.0,
                        current_bid: 0.0,
                        message: "Event channel closed".to_string(),
                    }),
                );
            }
            Err(_) => {
                return (
                    StatusCode::REQUEST_TIMEOUT,
                    Json(OrderPingResponse {
                        success: false,
                        ping_ms: 0.0,
                        order_place_ms: 0.0,
                        order_cancel_ms: 0.0,
                        total_ms: 0.0,
                        order_id: None,
                        test_price: 0.0,
                        current_bid: 0.0,
                        message: "No SOLUSDT data received in 5 seconds. Subscribe to bookticker first.".to_string(),
                    }),
                );
            }
        }
    };
    
    // ═══════════════════════════════════════════════════════════
    // 2. Рассчитываем безопасную цену (50% от bid - не исполнится)
    // ═══════════════════════════════════════════════════════════
    
    let safe_price = (current_bid * 0.5 * 100.0).round() / 100.0;
    
    tracing::info!(
        "Current bid: ${:.2}, Test price: ${:.2} (50% below)",
        current_bid,
        safe_price
    );
    
    // ═══════════════════════════════════════════════════════════
    // 3. Выставляем ордер и замеряем время
    // ═══════════════════════════════════════════════════════════
    
    let (place_tx, place_rx) = tokio::sync::oneshot::channel();
    let place_tx = Arc::new(tokio::sync::Mutex::new(Some(place_tx)));
    
    let start_place = Instant::now();
    
    app.trade_manager
        .send_limit_order(
            "ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm",
            "3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4",
            "SOLUSDT",
            safe_price,
            0.1,  // минимальный объем
            "BUY",
            move |resp: Value| {
                let tx_clone = place_tx.clone();
                tokio::spawn(async move {
                    if let Some(sender) = tx_clone.lock().await.take() {
                        let _ = sender.send(resp);
                    }
                });
            },
        )
        .await;
    
    // Ждём ответ на размещение
    let (order_id, order_place_ms) = match tokio::time::timeout(Duration::from_secs(10), place_rx).await {
        Ok(Ok(response)) => {
            let elapsed = start_place.elapsed().as_secs_f64() * 1000.0;
            
            if let Some(error) = response.get("error") {
                tracing::error!("❌ Order placement failed: {}", error);
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OrderPingResponse {
                        success: false,
                        ping_ms: elapsed,
                        order_place_ms: elapsed,
                        order_cancel_ms: 0.0,
                        total_ms: elapsed,
                        order_id: None,
                        test_price: safe_price,
                        current_bid,
                        message: format!("Order failed: {}", error),
                    }),
                );
            }
            
            if let Some(order_id) = response["result"]["orderId"].as_i64() {
                tracing::info!("✅ Order placed in {:.2}ms | ID: {}", elapsed, order_id);
                (order_id, elapsed)
            } else {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OrderPingResponse {
                        success: false,
                        ping_ms: elapsed,
                        order_place_ms: elapsed,
                        order_cancel_ms: 0.0,
                        total_ms: elapsed,
                        order_id: None,
                        test_price: safe_price,
                        current_bid,
                        message: "No orderId in response".to_string(),
                    }),
                );
            }
        }
        Ok(Err(_)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OrderPingResponse {
                    success: false,
                    ping_ms: 0.0,
                    order_place_ms: 0.0,
                    order_cancel_ms: 0.0,
                    total_ms: 0.0,
                    order_id: None,
                    test_price: safe_price,
                    current_bid,
                    message: "Channel error".to_string(),
                }),
            );
        }
        Err(_) => {
            return (
                StatusCode::REQUEST_TIMEOUT,
                Json(OrderPingResponse {
                    success: false,
                    ping_ms: 10000.0,
                    order_place_ms: 10000.0,
                    order_cancel_ms: 0.0,
                    total_ms: 10000.0,
                    order_id: None,
                    test_price: safe_price,
                    current_bid,
                    message: "Order placement timeout (10s)".to_string(),
                }),
            );
        }
    };
    
    // ═══════════════════════════════════════════════════════════
    // 4. Отменяем ордер и замеряем время
    // ═══════════════════════════════════════════════════════════
    
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let cancel_tx = Arc::new(tokio::sync::Mutex::new(Some(cancel_tx)));
    
    let start_cancel = Instant::now();
    
    app.trade_manager
        .cancel_limit_order(
            "ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm",
            "3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4",
            "SOLUSDT",
            &order_id.to_string(),
            move |resp: Value| {
                let tx_clone = cancel_tx.clone();
                tokio::spawn(async move {
                    if let Some(sender) = tx_clone.lock().await.take() {
                        let _ = sender.send(resp);
                    }
                });
            },
        )
        .await;
    
    // Ждём ответ на отмену
    let order_cancel_ms = match tokio::time::timeout(Duration::from_secs(10), cancel_rx).await {
        Ok(Ok(response)) => {
            let elapsed = start_cancel.elapsed().as_secs_f64() * 1000.0;
            
            if let Some(error) = response.get("error") {
                tracing::warn!("⚠️ Order cancellation failed: {}", error);
            } else {
                tracing::info!("✅ Order cancelled in {:.2}ms", elapsed);
            }
            
            elapsed
        }
        Ok(Err(_)) => 0.0,
        Err(_) => 10000.0,
    };
    
    // ═══════════════════════════════════════════════════════════
    // 5. Возвращаем результаты
    // ═══════════════════════════════════════════════════════════

    let total_ms = order_place_ms + order_cancel_ms;
    let ping_ms = order_place_ms;  // основной пинг - размещение ордера
    
    tracing::info!(
        "🏓 Ping test completed | Place: {:.2}ms | Cancel: {:.2}ms | Total: {:.2}ms",
        order_place_ms,
        order_cancel_ms,
        total_ms
    );
    
    (
        StatusCode::OK,
        Json(OrderPingResponse {
            success: true,
            ping_ms,
            order_place_ms,
            order_cancel_ms,
            total_ms,
            order_id: Some(order_id),
            test_price: safe_price,
            current_bid,
            message: format!(
                "Order placed and cancelled successfully. Latency: {:.2}ms",
                ping_ms
            ),
        }),
    )
}