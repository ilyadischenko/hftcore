// src/ffi_types.rs

/// C-совместимый Event для FFI и broadcast
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CEvent {
    pub event_type: u8,  // 0 = BookTicker, 1 = Trade
    pub data: CEventData,
    pub received_at_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union CEventData {
    pub book_ticker: CBookTicker,
    pub trade: CTrade,
}

impl std::fmt::Debug for CEventData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // По умолчанию выводим как "CEventData { ... }"
        // Нельзя знать какое поле активно без контекста
        write!(f, "CEventData {{ ... }}")
    }
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

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CTrade {
    pub symbol: [u8; 16],
    pub symbol_len: u8,
    pub price: f64,
    pub qty: f64,
    pub time: i64,
}

// Удобные методы для работы с C-типами
impl CBookTicker {
    pub fn symbol_str(&self) -> &str {
        unsafe {
            std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize])
        }
    }
    
    pub fn mid_price(&self) -> f64 {
        (self.bid_price + self.ask_price) / 2.0
    }
    
    pub fn spread(&self) -> f64 {
        self.ask_price - self.bid_price
    }
}

impl CTrade {
    pub fn symbol_str(&self) -> &str {
        unsafe {
            std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize])
        }
    }
    
    pub fn side(&self) -> &str {
        if self.qty > 0.0 { "BUY" } else { "SELL" }
    }
}