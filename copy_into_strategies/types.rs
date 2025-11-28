// copy_into_strategies/types.rs

use std::sync::atomic::{AtomicBool, Ordering};

// ═══════════════════════════════════════════════════════════
// EVENTS
// ═══════════════════════════════════════════════════════════

#[repr(C)]
pub union EventData {
    pub book_ticker: BookTickerData,
    pub trade: TradeData,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BookTickerData {
    pub symbol: [u8; 16],
    pub symbol_len: u8,
    pub bid_price: f64,
    pub bid_qty: f64,
    pub ask_price: f64,
    pub ask_qty: f64,
    pub update_id: u64,
    pub event_time: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TradeData {
    pub symbol: [u8; 16],
    pub symbol_len: u8,
    pub price: f64,
    pub qty: f64,
    pub trade_id: u64,
    pub event_time: u64,
    pub is_buyer_maker: bool,
}

#[repr(C)]
pub struct CEvent {
    pub event_type: u8,  // 0 = BookTicker, 1 = Trade
    pub data: EventData,
}

// ═══════════════════════════════════════════════════════════
// CONFIG
// ═══════════════════════════════════════════════════════════

#[repr(C)]
pub struct StrategyConfig {
    pub symbol: [u8; 32],
    pub symbol_len: u8,
    pub params_json: *const std::os::raw::c_char,
    pub stop_flag: *const AtomicBool,
}

impl StrategyConfig {
    pub fn symbol(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
    
    pub fn should_stop(&self) -> bool {
        unsafe { (*self.stop_flag).load(Ordering::Relaxed) }
    }
    
    pub fn params_raw(&self) -> &str {
        unsafe {
            std::ffi::CStr::from_ptr(self.params_json)
                .to_str()
                .unwrap_or("{}")
        }
    }
}

// ═══════════════════════════════════════════════════════════
// ORDERS
// ═══════════════════════════════════════════════════════════

#[repr(C)]
pub struct OrderResult {
    pub success: bool,
    pub order_id: i64,
    pub error_code: i32,
}

pub type PlaceOrderFn = unsafe extern "C" fn(
    symbol: *const u8,
    symbol_len: usize,
    side: u8,
    quantity: f64,
) -> OrderResult;

pub type CancelOrderFn = unsafe extern "C" fn(order_id: i64) -> OrderResult;