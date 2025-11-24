// Auto-generated types - DO NOT EDIT
// Copied from template at strategy creation
use std::os::raw::c_char;

#[repr(C)]
#[derive(Clone, Copy)]
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
}

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

impl CBookTicker {
    pub fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
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

impl CTrade {
    pub fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OrderResult {
    pub success: bool,
    pub order_id: i64,
    pub error_code: i32,
}

pub type OrderCallback = unsafe extern "C" fn(result: OrderResult);

pub type PlaceOrderFn = unsafe extern "C" fn(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    price: f64,
    quantity: f64,
    side: *const c_char,
    callback: OrderCallback,
);

pub type CancelOrderFn = unsafe extern "C" fn(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    order_id: i64,
    callback: OrderCallback,
);


#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StrategyConfig {
    pub symbol: [u8; 16],           // "SOLUSDT"
    pub symbol_len: u8,
    pub params_json: *const c_char, // JSON строка с параметрами
}

impl StrategyConfig {
    pub fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
    
    /// Парсинг JSON параметров в любую структуру
    pub fn parse_params<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        if self.params_json.is_null() {
            // Пустой JSON объект по умолчанию
            return serde_json::from_str("{}");
        }
        
        unsafe {
            let c_str = std::ffi::CStr::from_ptr(self.params_json);
            let json_str = c_str.to_str().unwrap_or("{}");
            serde_json::from_str(json_str)
        }
    }
}