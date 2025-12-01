// copy_into_strategies/types.rs

use std::ffi::c_char;
use std::sync::atomic::{AtomicBool, Ordering};

// ═══════════════════════════════════════════════════════════
// RAW FFI TYPES
// ═══════════════════════════════════════════════════════════

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CEvent {
    pub event_type: u8, 
    pub data: CEventData,
    pub received_at_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union CEventData {
    pub book_ticker: CBookTicker,
    pub trade: CTrade,
    pub order: COrderUpdate,
    pub account: CAccountUpdate,
}

// !!! ВОТ ЭТОГО НЕ ХВАТАЛО !!!
impl std::fmt::Debug for CEventData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CEventData {{ ... }}")
    }
}

// ─── Market Data ───

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CBookTicker {
    pub symbol: [u8; 16],
    pub symbol_len: u8,
    pub bid_price: f64,
    pub ask_price: f64,
    pub bid_qty: f64,
    pub ask_qty: f64,
    pub time: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CTrade {
    pub symbol: [u8; 16],
    pub symbol_len: u8,
    pub price: f64,
    pub qty: f64,
    pub time: i64,
}

// ─── User Data ───

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct COrderUpdate {
    pub symbol: [u8; 16],
    pub symbol_len: u8,
    
    pub client_order_id: [u8; 32],
    pub client_order_id_len: u8,
    
    pub order_id: i64,
    pub price: f64,
    pub qty: f64,
    pub avg_price: f64,
    pub accumulated_qty: f64,
    pub commission: f64,
    
    pub status_char: u8,
    pub side_char: u8,
    
    pub event_time: i64,
    pub trade_time: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CBalanceItem {
    pub asset: [u8; 8],
    pub asset_len: u8,
    pub wallet_balance: f64,
    pub cross_wallet_balance: f64,
    pub balance_change: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CAccountUpdate {
    pub event_time: i64,
    pub reason_code: u8,
    pub balances_count: u8,
    pub balances: [CBalanceItem; 10],
}

// ═══════════════════════════════════════════════════════════
// SAFE ACCESSORS
// ═══════════════════════════════════════════════════════════

impl CEvent {
    #[inline(always)]
    pub fn as_book_ticker(&self) -> Option<&CBookTicker> {
        if self.event_type == 0 { unsafe { Some(&self.data.book_ticker) } } else { None }
    }

    #[inline(always)]
    pub fn as_trade(&self) -> Option<&CTrade> {
        if self.event_type == 1 { unsafe { Some(&self.data.trade) } } else { None }
    }

    #[inline(always)]
    pub fn as_order(&self) -> Option<&COrderUpdate> {
        if self.event_type == 2 { unsafe { Some(&self.data.order) } } else { None }
    }

    #[inline(always)]
    pub fn as_account(&self) -> Option<&CAccountUpdate> {
        if self.event_type == 3 { unsafe { Some(&self.data.account) } } else { None }
    }

    #[inline(always)]
    pub fn as_funding(&self) -> Option<&CAccountUpdate> {
        if self.event_type == 3 { 
            unsafe {
                // Код 4 = FUNDING_FEE (согласно нашему парсеру)
                if self.data.account.reason_code == 4 {
                    return Some(&self.data.account);
                }
            }
        }
        None
    }
}

impl CBookTicker {
    pub fn symbol(&self) -> &str { unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) } }
}
impl CTrade {
    pub fn symbol(&self) -> &str { unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) } }
}
impl COrderUpdate {
    pub fn symbol(&self) -> &str { unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) } }
    pub fn client_id(&self) -> &str { unsafe { std::str::from_utf8_unchecked(&self.client_order_id[..self.client_order_id_len as usize]) } }
}
impl CBalanceItem {
    pub fn asset(&self) -> &str { unsafe { std::str::from_utf8_unchecked(&self.asset[..self.asset_len as usize]) } }
}

// ═══════════════════════════════════════════════════════════
// CALLBACK TYPES
// ═══════════════════════════════════════════════════════════

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OrderResult {
    pub success: bool,
    pub order_id: i64,
    pub error_code: i32,
}

pub type PlaceOrderFn = unsafe extern "C" fn(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    price: f64,
    quantity: f64,
    side: *const c_char,
    order_type: u8,
    reduce_only: bool,
    callback: unsafe extern "C" fn(OrderResult),
);

pub type CancelOrderFn = unsafe extern "C" fn(
    api_key: *const c_char, secret_key: *const c_char, symbol: *const c_char,
    order_id: i64, callback: unsafe extern "C" fn(OrderResult),
);

// ═══════════════════════════════════════════════════════════
// STRATEGY CONFIG
// ═══════════════════════════════════════════════════════════

#[repr(C)]
pub struct StrategyConfig {
    pub symbol: [u8; 32],
    pub symbol_len: u8,
    pub params_json: *const c_char,
    pub stop_flag: *const AtomicBool,
    pub _reserved: *const std::ffi::c_void,
}

impl StrategyConfig {
    pub fn symbol(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
    pub fn should_stop(&self) -> bool {
        unsafe { (*self.stop_flag).load(Ordering::Relaxed) }
    }
    pub fn params_str(&self) -> &str {
        unsafe { std::ffi::CStr::from_ptr(self.params_json).to_str().unwrap_or("{}") }
    }
}