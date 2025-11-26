// src/strategies/trading.rs

use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::{Arc, OnceLock};
use crate::exchange_trade::ExchangeTrade;

// ═══════════════════════════════════════════════════════════
// ГЛОБАЛЬНЫЙ TRADE MANAGER (инициализируется один раз)
// ═══════════════════════════════════════════════════════════

static TRADE_MANAGER: OnceLock<Arc<ExchangeTrade>> = OnceLock::new();

pub fn init_trading(manager: Arc<ExchangeTrade>) {
    TRADE_MANAGER.set(manager).ok();
}

// ═══════════════════════════════════════════════════════════
// C-ТИПЫ
// ═══════════════════════════════════════════════════════════

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OrderResult {
    pub success: bool,
    pub order_id: i64,
    pub error_code: i32,
}

pub type OrderCallback = unsafe extern "C" fn(result: OrderResult);

// ═══════════════════════════════════════════════════════════
// FFI ФУНКЦИИ (экспортируются в DLL)
// ═══════════════════════════════════════════════════════════

#[no_mangle]
pub unsafe extern "C" fn place_order(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    price: f64,
    quantity: f64,
    side: *const c_char,
    order_type: u8,        // 0 = LIMIT, 1 = MARKET
    callback: OrderCallback,
) {
    let manager = TRADE_MANAGER.get().expect("Trading not initialized");
    
    let api_key = CStr::from_ptr(api_key).to_str().unwrap();
    let secret_key = CStr::from_ptr(secret_key).to_str().unwrap();
    let symbol = CStr::from_ptr(symbol).to_str().unwrap();
    let side = CStr::from_ptr(side).to_str().unwrap();

    
    let manager = manager.clone();
    tokio::spawn(async move {
        // Общий обработчик ответа
        let handle_resp = move |resp: serde_json::Value| {
            let result = if let Some(error) = resp.get("error") {
                OrderResult {
                    success: false,
                    order_id: -1,
                    error_code: error["code"].as_i64().unwrap_or(-1) as i32,
                }
            } else if let Some(order_id) = resp["result"]["orderId"].as_i64() {
                OrderResult {
                    success: true,
                    order_id,
                    error_code: 0,
                }
            } else {
                OrderResult {
                    success: false,
                    order_id: -1,
                    error_code: -9998,
                }
            };
            unsafe { callback(result); }
        };

        if order_type == 1 {
            // MARKET
            manager
                .send_market_order(
                    api_key,
                    secret_key,
                    symbol,
                    quantity,
                    side,
                    handle_resp,
                )
                .await;
        } else {
            // LIMIT (по умолчанию)
            manager
                .send_limit_order(
                    api_key,
                    secret_key,
                    symbol,
                    price,
                    quantity,
                    side,
                    handle_resp,
                )
                .await;
        }
    });
}

#[no_mangle]
pub unsafe extern "C" fn cancel_order(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    order_id: i64,
    callback: OrderCallback,
) {
    let manager = TRADE_MANAGER.get().expect("Trading not initialized");
    
    let api_key = CStr::from_ptr(api_key).to_str().unwrap();
    let secret_key = CStr::from_ptr(secret_key).to_str().unwrap();
    let symbol = CStr::from_ptr(symbol).to_str().unwrap();
    
    let manager = manager.clone();
    tokio::spawn(async move {
        manager.cancel_limit_order(
            api_key, secret_key, symbol, &order_id.to_string(),
            move |resp| {
                let result = if resp.get("error").is_some() {
                    OrderResult {
                        success: false,
                        order_id: -1,
                        error_code: resp["error"]["code"].as_i64().unwrap_or(-1) as i32,
                    }
                } else {
                    OrderResult { success: true, order_id, error_code: 0 }
                };
                unsafe { callback(result); }
            },
        ).await;
    });
}



pub type PlaceOrderFn = unsafe extern "C" fn(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    price: f64,
    quantity: f64,
    side: *const c_char,
    order_type: u8,        // 0 = LIMIT, 1 = MARKET
    callback: OrderCallback,
);

pub type CancelOrderFn = unsafe extern "C" fn(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    order_id: i64,
    callback: OrderCallback,
);