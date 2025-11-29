// src/user_data/parser.rs

use super::types::*;
use serde_json::Value;

pub fn parse_message(text: &str) -> Option<UserDataEvent> {
    let json: Value = serde_json::from_str(text).ok()?;
    
    match json.get("e")?.as_str()? {
        "ORDER_TRADE_UPDATE" => parse_order_update(&json),
        "ACCOUNT_UPDATE" => parse_account_update(&json),
        "MARGIN_CALL" => parse_margin_call(&json),
        _ => Some(UserDataEvent::Unknown { raw: text.to_string() }),
    }
}

fn parse_order_update(json: &Value) -> Option<UserDataEvent> {
    let o = json.get("o")?;
    
    let order = OrderInfo {
        symbol: o.get("s")?.as_str()?.to_string(),
        order_id: o.get("i")?.as_i64()?,
        client_order_id: o.get("c")?.as_str()?.to_string(),
        side: match o.get("S")?.as_str()? {
            "BUY" => OrderSide::Buy,
            _ => OrderSide::Sell,
        },
        order_type: match o.get("o")?.as_str()? {
            "LIMIT" => OrderType::Limit,
            "MARKET" => OrderType::Market,
            "STOP" => OrderType::Stop,
            "STOP_MARKET" => OrderType::StopMarket,
            "TAKE_PROFIT" => OrderType::TakeProfit,
            "TAKE_PROFIT_MARKET" => OrderType::TakeProfitMarket,
            "TRAILING_STOP_MARKET" => OrderType::TrailingStopMarket,
            _ => OrderType::Other,
        },
        status: match o.get("X")?.as_str()? {
            "NEW" => OrderStatus::New,
            "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled,
            "FILLED" => OrderStatus::Filled,
            "CANCELED" => OrderStatus::Canceled,
            "REJECTED" => OrderStatus::Rejected,
            "EXPIRED" => OrderStatus::Expired,
            _ => OrderStatus::Unknown,
        },
        price: parse_f64(o, "p"),
        quantity: parse_f64(o, "q"),
        stop_price: parse_f64(o, "sp"),
        time_in_force: o.get("f").and_then(|v| v.as_str()).unwrap_or("GTC").to_string(),
        last_filled_qty: parse_f64(o, "l"),
        last_filled_price: parse_f64(o, "L"),
        cumulative_qty: parse_f64(o, "z"),
        average_price: parse_f64(o, "ap"),
        commission: parse_f64(o, "n"),
        commission_asset: o.get("N").and_then(|v| v.as_str()).map(String::from),
        order_time: o.get("T")?.as_u64()?,
        trade_time: o.get("T").and_then(|v| v.as_u64()).unwrap_or(0),
        trade_id: o.get("t").and_then(|v| v.as_i64()).unwrap_or(0),
        realized_pnl: parse_f64(o, "rp"),
    };
    
    Some(UserDataEvent::OrderUpdate(OrderUpdateEvent {
        event_time: json.get("E")?.as_u64()?,
        transaction_time: json.get("T")?.as_u64()?,
        order,
    }))
}

fn parse_account_update(json: &Value) -> Option<UserDataEvent> {
    let a = json.get("a")?;
    
    let reason = match a.get("m")?.as_str()? {
        "DEPOSIT" => UpdateReason::Deposit,
        "WITHDRAW" => UpdateReason::Withdraw,
        "ORDER" => UpdateReason::Order,
        "FUNDING_FEE" => UpdateReason::FundingFee,
        "WITHDRAW_REJECT" => UpdateReason::WithdrawReject,
        "ADJUSTMENT" => UpdateReason::Adjustment,
        "INSURANCE_CLEAR" => UpdateReason::InsuranceClear,
        "ADMIN_DEPOSIT" => UpdateReason::AdminDeposit,
        "ADMIN_WITHDRAW" => UpdateReason::AdminWithdraw,
        "MARGIN_TRANSFER" => UpdateReason::MarginTransfer,
        "MARGIN_TYPE_CHANGE" => UpdateReason::MarginTypeChange,
        "ASSET_TRANSFER" => UpdateReason::AssetTransfer,
        "OPTIONS_PREMIUM_FEE" => UpdateReason::OptionsPremiumFee,
        "OPTIONS_SETTLE_PROFIT" => UpdateReason::OptionsSettleProfit,
        "AUTO_EXCHANGE" => UpdateReason::AutoExchange,
        _ => UpdateReason::Unknown,
    };
    
    let balances: Vec<BalanceInfo> = a.get("B")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter().filter_map(|b| {
                Some(BalanceInfo {
                    asset: b.get("a")?.as_str()?.to_string(),
                    wallet_balance: parse_f64(b, "wb"),
                    cross_wallet_balance: parse_f64(b, "cw"),
                    balance_change: parse_f64(b, "bc"),
                })
            }).collect()
        })
        .unwrap_or_default();
    
    let positions: Vec<PositionInfo> = a.get("P")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter().filter_map(|p| {
                Some(PositionInfo {
                    symbol: p.get("s")?.as_str()?.to_string(),
                    position_amount: parse_f64(p, "pa"),
                    entry_price: parse_f64(p, "ep"),
                    unrealized_pnl: parse_f64(p, "up"),
                    margin_type: p.get("mt")?.as_str()?.to_string(),
                    isolated_wallet: parse_f64(p, "iw"),
                    position_side: p.get("ps")?.as_str()?.to_string(),
                })
            }).collect()
        })
        .unwrap_or_default();
    
    Some(UserDataEvent::AccountUpdate(AccountUpdateEvent {
        event_time: json.get("E")?.as_u64()?,
        transaction_time: json.get("T")?.as_u64()?,
        reason,
        balances,
        positions,
    }))
}

fn parse_margin_call(json: &Value) -> Option<UserDataEvent> {
    let positions: Vec<MarginCallPosition> = json.get("p")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter().filter_map(|p| {
                Some(MarginCallPosition {
                    symbol: p.get("s")?.as_str()?.to_string(),
                    position_side: p.get("ps")?.as_str()?.to_string(),
                    position_amount: parse_f64(p, "pa"),
                    margin_type: p.get("mt")?.as_str()?.to_string(),
                    mark_price: parse_f64(p, "mp"),
                    unrealized_pnl: parse_f64(p, "up"),
                    maintenance_margin: parse_f64(p, "mm"),
                })
            }).collect()
        })
        .unwrap_or_default();
    
    Some(UserDataEvent::MarginCall(MarginCallEvent {
        event_time: json.get("E")?.as_u64()?,
        cross_wallet_balance: parse_f64(json, "cw"),
        positions,
    }))
}

fn parse_f64(v: &Value, key: &str) -> f64 {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}