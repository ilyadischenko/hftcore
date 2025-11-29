// src/user_data/types.rs

use serde::{Deserialize, Serialize};

/// Все события из User Data Stream
#[derive(Debug, Clone, Serialize)]
pub enum UserDataEvent {
    /// Ордер создан/изменён/исполнен
    OrderUpdate(OrderUpdateEvent),
    
    /// Изменение баланса/позиции (включая funding)
    AccountUpdate(AccountUpdateEvent),
    
    /// Margin call
    MarginCall(MarginCallEvent),
    
    /// Не распознанное событие
    Unknown { raw: String },
}

// ═══════════════════════════════════════════════════════════
// ORDER UPDATE — детальная информация по ордерам
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderUpdateEvent {
    pub event_time: u64,
    pub transaction_time: u64,
    pub order: OrderInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderInfo {
    pub symbol: String,
    pub order_id: i64,
    pub client_order_id: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub status: OrderStatus,
    pub price: f64,
    pub quantity: f64,
    pub stop_price: f64,
    pub time_in_force: String,
    
    // Execution details
    pub last_filled_qty: f64,
    pub last_filled_price: f64,
    pub cumulative_qty: f64,
    pub average_price: f64,
    
    // Commission
    pub commission: f64,
    pub commission_asset: Option<String>,
    
    // Timestamps
    pub order_time: u64,
    pub trade_time: u64,
    pub trade_id: i64,
    
    // PnL
    pub realized_pnl: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderType {
    Limit,
    Market,
    Stop,
    StopMarket,
    TakeProfit,
    TakeProfitMarket,
    TrailingStopMarket,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderStatus {
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
    Expired,
    #[serde(other)]
    Unknown,
}

// ═══════════════════════════════════════════════════════════
// ACCOUNT UPDATE — балансы, позиции, funding
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountUpdateEvent {
    pub event_time: u64,
    pub transaction_time: u64,
    pub reason: UpdateReason,
    pub balances: Vec<BalanceInfo>,
    pub positions: Vec<PositionInfo>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UpdateReason {
    Deposit,
    Withdraw,
    Order,
    FundingFee,  // ← Это для отслеживания funding!
    WithdrawReject,
    Adjustment,
    InsuranceClear,
    AdminDeposit,
    AdminWithdraw,
    MarginTransfer,
    MarginTypeChange,
    AssetTransfer,
    OptionsPremiumFee,
    OptionsSettleProfit,
    AutoExchange,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceInfo {
    pub asset: String,
    pub wallet_balance: f64,
    pub cross_wallet_balance: f64,
    pub balance_change: f64,  // ← Сумма изменения (funding amount)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionInfo {
    pub symbol: String,
    pub position_amount: f64,
    pub entry_price: f64,
    pub unrealized_pnl: f64,
    pub margin_type: String,
    pub isolated_wallet: f64,
    pub position_side: String,
}

// ═══════════════════════════════════════════════════════════
// MARGIN CALL
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginCallEvent {
    pub event_time: u64,
    pub cross_wallet_balance: f64,
    pub positions: Vec<MarginCallPosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginCallPosition {
    pub symbol: String,
    pub position_side: String,
    pub position_amount: f64,
    pub margin_type: String,
    pub mark_price: f64,
    pub unrealized_pnl: f64,
    pub maintenance_margin: f64,
}

// ═══════════════════════════════════════════════════════════
// STREAM INFO (для API ответов)
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct StreamInfo {
    pub api_key_hash: String,
    pub connected: bool,
    pub connected_at: i64,
    pub last_event_at: Option<i64>,
    pub event_count: u64,
    pub order_count: u64,
    pub funding_count: u64,
}