// copy_into_strategies/types.rs

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

// ═══════════════════════════════════════════════════════════
// MARKET DATA EVENTS (существующие)
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
    pub event_type: u8,
    pub data: EventData,
}

// ═══════════════════════════════════════════════════════════
// USER DATA EVENTS (новые)
// ═══════════════════════════════════════════════════════════

/// События из User Data Stream
#[derive(Debug, Clone)]
pub enum UserDataEvent {
    /// Ордер создан/изменён/исполнен
    Order(OrderEvent),
    
    /// Funding fee начислен
    FundingFee(FundingEvent),
    
    /// Другие изменения аккаунта
    AccountChange(AccountChangeEvent),
    
    /// Неизвестное событие
    Other,
}

#[derive(Debug, Clone)]
pub struct OrderEvent {
    pub symbol: String,
    pub order_id: i64,
    pub client_order_id: String,
    pub side: String,           // "BUY" / "SELL"
    pub order_type: String,     // "MARKET" / "LIMIT"
    pub status: String,         // "NEW" / "FILLED" / "CANCELED"
    pub price: f64,
    pub quantity: f64,
    pub filled_qty: f64,
    pub average_price: f64,
    pub commission: f64,
    pub realized_pnl: f64,
    pub event_time: u64,
}

#[derive(Debug, Clone)]
pub struct FundingEvent {
    pub asset: String,
    pub amount: f64,            // + = получили, - = заплатили
    pub wallet_balance: f64,
    pub event_time: u64,
}

#[derive(Debug, Clone)]
pub struct AccountChangeEvent {
    pub reason: String,         // "ORDER", "DEPOSIT", "WITHDRAW"
    pub asset: String,
    pub balance_change: f64,
    pub event_time: u64,
}

// ═══════════════════════════════════════════════════════════
// ORDER CALLBACKS
// ═══════════════════════════════════════════════════════════

#[repr(C)]
pub struct OrderResult {
    pub success: bool,
    pub order_id: i64,
    pub error_code: i32,
}

pub type PlaceOrderFn = unsafe extern "C" fn(
    api_key: *const std::os::raw::c_char,
    secret_key: *const std::os::raw::c_char,
    symbol: *const std::os::raw::c_char,
    price: f64,
    quantity: f64,
    side: *const std::os::raw::c_char,
    order_type: u8,
    callback: unsafe extern "C" fn(OrderResult),
) -> ();

pub type CancelOrderFn = unsafe extern "C" fn(order_id: i64) -> OrderResult;

// ═══════════════════════════════════════════════════════════
// STRATEGY CONFIG
// ═══════════════════════════════════════════════════════════

// Приватный тип для user_data receiver
type UserDataReceiver = Mutex<Option<tokio::sync::broadcast::Receiver<UserDataEvent>>>;

#[repr(C)]
pub struct StrategyConfig {
    pub symbol: [u8; 32],
    pub symbol_len: u8,
    pub params_json: *const std::os::raw::c_char,
    pub stop_flag: *const AtomicBool,
    pub user_data_rx: *const UserDataReceiver,
}

impl StrategyConfig {
    /// Получить символ
    pub fn symbol(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
    
    /// Проверить флаг остановки
    pub fn should_stop(&self) -> bool {
        unsafe { (*self.stop_flag).load(Ordering::Relaxed) }
    }
    
    /// Получить raw JSON параметров
    pub fn params_raw(&self) -> &str {
        unsafe {
            std::ffi::CStr::from_ptr(self.params_json)
                .to_str()
                .unwrap_or("{}")
        }
    }
    
    /// Попытаться получить User Data событие (неблокирующий)
    pub fn try_recv_user_data(&self) -> Option<UserDataEvent> {
        if self.user_data_rx.is_null() {
            return None;
        }
        
        unsafe {
            let mutex = &*self.user_data_rx;
            if let Ok(mut guard) = mutex.try_lock() {
                if let Some(ref mut rx) = *guard {
                    // Конвертируем из внутреннего типа
                    if let Ok(event) = rx.try_recv() {
                        return Some(convert_user_event(event));
                    }
                }
            }
        }
        None
    }
    
    /// Есть ли подключённый User Data Stream
    pub fn has_user_data(&self) -> bool {
        if self.user_data_rx.is_null() {
            return false;
        }
        
        unsafe {
            let mutex = &*self.user_data_rx;
            if let Ok(guard) = mutex.try_lock() {
                return guard.is_some();
            }
        }
        false
    }
}

// Функция конвертации (упрощённая, в реальности нужен полный маппинг)
fn convert_user_event(event: crate::user_data::UserDataEvent) -> UserDataEvent {
    match event {
        crate::user_data::UserDataEvent::OrderUpdate(ou) => {
            UserDataEvent::Order(OrderEvent {
                symbol: ou.order.symbol,
                order_id: ou.order.order_id,
                client_order_id: ou.order.client_order_id,
                side: format!("{:?}", ou.order.side),
                order_type: format!("{:?}", ou.order.order_type),
                status: format!("{:?}", ou.order.status),
                price: ou.order.price,
                quantity: ou.order.quantity,
                filled_qty: ou.order.cumulative_qty,
                average_price: ou.order.average_price,
                commission: ou.order.commission,
                realized_pnl: ou.order.realized_pnl,
                event_time: ou.event_time,
            })
        }
        crate::user_data::UserDataEvent::AccountUpdate(au) 
            if au.reason == crate::user_data::UpdateReason::FundingFee => 
        {
            let b = au.balances.first().cloned().unwrap_or_default();
            UserDataEvent::FundingFee(FundingEvent {
                asset: b.asset,
                amount: b.balance_change,
                wallet_balance: b.wallet_balance,
                event_time: au.event_time,
            })
        }
        crate::user_data::UserDataEvent::AccountUpdate(au) => {
            let b = au.balances.first().cloned().unwrap_or_default();
            UserDataEvent::AccountChange(AccountChangeEvent {
                reason: format!("{:?}", au.reason),
                asset: b.asset,
                balance_change: b.balance_change,
                event_time: au.event_time,
            })
        }
        _ => UserDataEvent::Other,
    }
}