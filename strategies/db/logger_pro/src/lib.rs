mod types;
use types::*;

use crossbeam::channel::{Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use chrono::{DateTime, Utc};

// Хелпер для красивого времени
fn format_ts(ts_ms: i64) -> String {
    if ts_ms <= 0 { return "N/A".to_string(); }
    let d = SystemTime::UNIX_EPOCH + Duration::from_millis(ts_ms as u64);
    let dt: DateTime<Utc> = d.into();
    dt.format("%H:%M:%S%.3f").to_string()
}

#[no_mangle]
pub unsafe extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    _place: PlaceOrderFn,
    _cancel: CancelOrderFn,
    config: StrategyConfig
) -> i32 {
    let rx = &*rx_ptr;
    let symbol = config.symbol();
    println!("\n🟢 [LOGGER PRO] Watching {}...", symbol);

    loop {
        if config.should_stop() { break; }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                // Вычисляем задержку (Latency)
                let now_ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
                let latency_us = (now_ns.saturating_sub(event.received_at_ns)) / 1000;

                match event.event_type {
                    // 0 = BookTicker
                    0 => {
                         // if let Some(bt) = event.as_book_ticker() {
                         //    println!("📖 TICK | {} | Bid: {:.2} Ask: {:.2}", format_ts(bt.time), bt.bid_price, bt.ask_price);
                         // }
                    },
                    
                    // 1 = Trade
                    1 => {
                        if let Some(t) = event.as_trade() {
                            println!("💰 TRADE | {} | {:<4} | P={:<8} Q={:<6} | Latency: {}us", 
                                format_ts(t.time), 
                                if t.qty > 0.0 { "BUY" } else { "SELL" },
                                t.price, 
                                t.qty.abs(),
                                latency_us
                            );
                        }
                    },
                    
                    // 2 = Order Update
                    2 => {
                        if let Some(o) = event.as_order() {
                            println!("📦 ORDER | {} | ID={} | {} {} | Status={} | Filled={} | Latency: {}us", 
                                format_ts(o.event_time),
                                o.order_id,
                                o.side_char as char, 
                                o.qty,
                                o.status_char as char,
                                o.accumulated_qty,
                                latency_us
                            );
                        }
                    },
                    
                    // 3 = Account Update (включая Фандинг)
                    3 => {
                        // Сначала проверяем, фандинг ли это?
                        if let Some(f) = event.as_funding() {
                            println!("💸 FUNDING DETECTED! | {}", format_ts(f.event_time));
                            for i in 0..f.balances_count {
                                let b = f.balances[i as usize];
                                let change = b.balance_change;
                                let sign = if change > 0.0 { "+" } else { "" };
                                println!("   >>> {}: {}{:.4} (Wallet: {:.2})", 
                                    b.asset(), sign, change, b.wallet_balance);
                            }
                        } 
                        // Если не фандинг, то просто обновление баланса
                        else if let Some(a) = event.as_account() {
                            println!("🏦 ACCOUNT (Reason: {}) | {}", a.reason_code, format_ts(a.event_time));
                             for i in 0..a.balances_count {
                                let b = a.balances[i as usize];
                                if b.balance_change != 0.0 {
                                    println!("   - {}: Change={}", b.asset(), b.balance_change);
                                }
                            }
                        }
                    },
                    
                    _ => {}
                }
            },
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    println!("🔴 [LOGGER PRO] STOPPED");
    0
}