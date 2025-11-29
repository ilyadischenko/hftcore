// src/user_data/parser.rs

use simd_json::serde as simd_serde;
use serde::Deserialize;
use crate::ffi_types::*;

// ═══════════════════════════════════════════════════════════
// ВРЕМЕННЫЕ СТРУКТУРЫ (Zero-copy маппинг)
// ═══════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct RawOrderEvent {
    #[serde(rename = "E")] event_time: i64,
    #[serde(rename = "o")] order: RawOrderData,
}

#[derive(Deserialize)]
struct RawOrderData {
    #[serde(rename = "s")] symbol: String,
    #[serde(rename = "c")] client_id: String,
    #[serde(rename = "S")] side: String,
    #[serde(rename = "X")] status: String,
    #[serde(rename = "i")] order_id: i64,
    #[serde(rename = "q")] original_qty: String,
    #[serde(rename = "p")] original_price: String,
    #[serde(rename = "ap")] avg_price: String,
    #[serde(rename = "z")] accumulated_qty: String,
    #[serde(rename = "n")] commission: Option<String>,
    #[serde(rename = "T")] trade_time: i64,
}

#[derive(Deserialize)]
struct RawAccountEvent {
    #[serde(rename = "E")] event_time: i64,
    #[serde(rename = "a")] data: RawAccountData,
}

#[derive(Deserialize)]
struct RawAccountData {
    #[serde(rename = "m")] reason: String,
    #[serde(rename = "B")] balances: Vec<RawBalance>,
}

#[derive(Deserialize)]
struct RawBalance {
    #[serde(rename = "a")] asset: String,
    #[serde(rename = "wb")] wallet_balance: String,
    #[serde(rename = "cw")] cross_wallet_balance: String,
    #[serde(rename = "bc")] balance_change: String,
}

// ═══════════════════════════════════════════════════════════
// PARSER ENTRY POINT
// ═══════════════════════════════════════════════════════════

/// Парсит входящий JSON-буфер и возвращает CEvent.
/// ВНИМАНИЕ: Буфер `buffer` будет модифицирован simd_json!
pub fn parse_to_c_event(buffer: &mut [u8]) -> Option<CEvent> {
    // Быстрая проверка типа события (сканирование байт без парсинга)
    if has_substring(buffer, b"ORDER_TRADE_UPDATE") {
        parse_order(buffer)
    } else if has_substring(buffer, b"ACCOUNT_UPDATE") {
        parse_account(buffer)
    } else {
        // Остальные события (MarginCall и т.д.) пока игнорируем для скорости
        None
    }
}

fn parse_order(buffer: &mut [u8]) -> Option<CEvent> {
    // simd_serde::from_slice использует вектор инструкций CPU для парсинга
    let raw: RawOrderEvent = simd_serde::from_slice(buffer).ok()?;
    
    let mut c_order = COrderUpdate {
        symbol: [0; 16],
        symbol_len: 0,
        client_order_id: [0; 32],
        client_order_id_len: 0,
        order_id: raw.order.order_id,
        price: parse_f64(&raw.order.original_price),
        qty: parse_f64(&raw.order.original_qty),
        avg_price: parse_f64(&raw.order.avg_price),
        accumulated_qty: parse_f64(&raw.order.accumulated_qty),
        commission: raw.order.commission.as_deref().map(parse_f64).unwrap_or(0.0),
        
        // Берем первую букву статуса и стороны ('N', 'F', 'C' / 'B', 'S')
        status_char: raw.order.status.as_bytes().first().copied().unwrap_or(b'?'),
        side_char: raw.order.side.as_bytes().first().copied().unwrap_or(b'?'),
        
        event_time: raw.event_time,
        trade_time: raw.order.trade_time,
    };

    // Копируем строки в фиксированные буферы
    copy_str(&mut c_order.symbol, &mut c_order.symbol_len, &raw.order.symbol);
    copy_str(&mut c_order.client_order_id, &mut c_order.client_order_id_len, &raw.order.client_id);

    Some(CEvent {
        event_type: 2, // 2 = User Order Update
        data: CEventData { order: c_order },
        received_at_ns: get_current_ts_ns(),
    })
}

fn parse_account(buffer: &mut [u8]) -> Option<CEvent> {
    let raw: RawAccountEvent = simd_serde::from_slice(buffer).ok()?;
    
    // Если балансы не менялись (пустой массив), событие не нужно стратегии
    if raw.data.balances.is_empty() {
        return None;
    }

    let mut c_acc = CAccountUpdate {
        event_time: raw.event_time,
        reason_code: map_reason(&raw.data.reason),
        balances_count: 0,
        balances: [CBalanceItem {
            asset: [0; 8], asset_len: 0,
            wallet_balance: 0.0, cross_wallet_balance: 0.0, balance_change: 0.0
        }; 10],
    };

    // Заполняем до 10 изменений баланса
    let count = raw.data.balances.len().min(10);
    c_acc.balances_count = count as u8;

    for i in 0..count {
        let src = &raw.data.balances[i];
        let dst = &mut c_acc.balances[i];
        
        copy_str(&mut dst.asset, &mut dst.asset_len, &src.asset);
        dst.wallet_balance = parse_f64(&src.wallet_balance);
        dst.cross_wallet_balance = parse_f64(&src.cross_wallet_balance);
        dst.balance_change = parse_f64(&src.balance_change);
    }

    Some(CEvent {
        event_type: 3, // 3 = User Account Update
        data: CEventData { account: c_acc },
        received_at_ns: get_current_ts_ns(),
    })
}

// ═══════════════════════════════════════════════════════════
// HELPERS (Inlined for speed)
// ═══════════════════════════════════════════════════════════

#[inline(always)]
fn has_substring(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[inline(always)]
fn parse_f64(s: &str) -> f64 {
    // Обычный parse достаточно быстр для строк, уже выделенных в JSON
    s.parse().unwrap_or(0.0)
}

#[inline(always)]
fn copy_str<const N: usize>(dst: &mut [u8; N], len: &mut u8, src: &str) {
    let bytes = src.as_bytes();
    *len = bytes.len().min(N) as u8;
    dst[..*len as usize].copy_from_slice(&bytes[..*len as usize]);
}

#[inline(always)]
fn get_current_ts_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn map_reason(r: &str) -> u8 {
    match r {
        "DEPOSIT" => 1,
        "WITHDRAW" => 2,
        "ORDER" => 3,
        "FUNDING_FEE" => 4,
        "WITHDRAW_REJECT" => 5,
        "ADJUSTMENT" => 6,
        "INSURANCE_CLEAR" => 7,
        "ADMIN_DEPOSIT" => 8,
        "ADMIN_WITHDRAW" => 9,
        "MARGIN_TRANSFER" => 10,
        "MARGIN_TYPE_CHANGE" => 11,
        "ASSET_TRANSFER" => 12,
        "OPTIONS_PREMIUM_FEE" => 13,
        "OPTIONS_SETTLE_PROFIT" => 14,
        "AUTO_EXCHANGE" => 15,
        _ => 0,
    }
}