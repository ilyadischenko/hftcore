mod types;
use types::*;

use crossbeam::channel::{Receiver, RecvTimeoutError};
use std::time::Duration;

#[no_mangle]
pub unsafe extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    _place: PlaceOrderFn,
    _cancel: CancelOrderFn,
    config: StrategyConfig
) -> i32 {
    let rx = &*rx_ptr;
    let symbol = config.symbol();
    println!("🟢 [LOGGER] STARTED watching {}", symbol);

    loop {
        if config.should_stop() { break; }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                match event.event_type {
                    0 => { // BookTicker
                         if let Some(bt) = event.as_book_ticker() {
                            // Раскомментируй для дебага тиков:
                            println!("📖 TICK: {} {}/{} t={}", bt.symbol(), bt.bid_price, bt.ask_price, bt.time);
                         }
                    },
                    1 => { // Trade
                        if let Some(t) = event.as_trade() {
                            println!("💰 TRADE: {} p={} q={}", t.symbol(), t.price, t.qty);
                        }
                    },
                    2 => { // ORDER
                        if let Some(o) = event.as_order() {
                            println!("📦 ORDER: {} ID={} Side={} Status={} Qty={} Filled={}", 
                                o.symbol(), o.order_id, o.side_char as char, 
                                o.status_char as char, o.qty, o.accumulated_qty);
                        }
                    },
                    3 => { // ACCOUNT
                        if let Some(a) = event.as_account() {
                            println!("🏦 ACCOUNT (Reason: {}):", a.reason_code);
                            for i in 0..a.balances_count {
                                let b = a.balances[i as usize];
                                println!("   - {}: Wallet={} Change={}", b.asset(), b.wallet_balance, b.balance_change);
                            }
                        }
                    },
                    _ => println!("❓ UNKNOWN EVENT TYPE: {}", event.event_type)
                }
            },
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    println!("🔴 [LOGGER] STOPPED");
    0
}