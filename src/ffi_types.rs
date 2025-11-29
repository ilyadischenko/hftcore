use std::ffi::c_char;

// ═══════════════════════════════════════════════════════════
// ГЛАВНОЕ СОБЫТИЕ
// ═══════════════════════════════════════════════════════════

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CEvent {
    /// Тип события:
    /// 0 = BookTicker (рынок)
    /// 1 = Trade (рынок)
    /// 2 = OrderUpdate (юзер)
    /// 3 = AccountUpdate (юзер)
    pub event_type: u8, 
    pub data: CEventData,
    pub received_at_ns: u64,
}

/// Объединение всех возможных данных.
/// Занимает память по размеру самого большого поля (AccountUpdate ~300 байт).
#[repr(C)]
#[derive(Clone, Copy)]
pub union CEventData {
    pub book_ticker: CBookTicker,
    pub trade: CTrade,
    pub order: COrderUpdate,
    pub account: CAccountUpdate,
}

// Для отладки (так как union по умолчанию не имеет Debug)
impl std::fmt::Debug for CEventData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CEventData {{ ... }}")
    }
}

// ═══════════════════════════════════════════════════════════
// MARKET DATA (как было)
// ═══════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════
// USER DATA (Новое!)
// ═══════════════════════════════════════════════════════════

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
    
    /// Статус ордера (первая буква):
    /// 'N' = New
    /// 'P' = Partially Filled
    /// 'F' = Filled
    /// 'C' = Canceled
    /// 'R' = Rejected
    /// 'E' = Expired
    pub status_char: u8, 
    
    /// Сторона: 'B' = Buy, 'S' = Sell
    pub side_char: u8,
    
    pub event_time: i64,
    pub trade_time: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CBalanceItem {
    pub asset: [u8; 8],  // Например "USDT"
    pub asset_len: u8,
    pub wallet_balance: f64,
    pub cross_wallet_balance: f64,
    pub balance_change: f64,
}

/// Обновление баланса.
/// Используем фиксированный массив на 10 элементов.
/// Обычно в одном событии меняется 1-2 ассета (USDT и монета).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CAccountUpdate {
    pub event_time: i64,
    
    /// Причина обновления (маппим в число):
    /// 0=Unknown, 1=Deposit, 2=Withdraw, 3=Order, 4=FundingFee ...
    pub reason_code: u8, 
    
    pub balances_count: u8,
    pub balances: [CBalanceItem; 10],
}

// ═══════════════════════════════════════════════════════════
// HELPERS (Rust-only удобства)
// ═══════════════════════════════════════════════════════════

impl CBookTicker {
    pub fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
}

impl CTrade {
    pub fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
}

impl COrderUpdate {
    pub fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
    pub fn client_id_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.client_order_id[..self.client_order_id_len as usize]) }
    }
}