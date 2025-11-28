mod types;
use types::*;

use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(true);

#[no_mangle]
pub extern "C" fn run(
    rx: *mut crossbeam::channel::Receiver<CEvent>,
    _place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32 {
    let rx = unsafe { &*rx };
    let symbol = unsafe { std::str::from_utf8_unchecked(&config.symbol[..config.symbol_len as usize]) };
    println!("Started on {}", symbol);
    
    while RUNNING.load(Ordering::Relaxed) {
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(event) if event.event_type == 0 => {
                let bt = unsafe { &event.data.book_ticker };
                let s = unsafe { std::str::from_utf8_unchecked(&bt.symbol[..bt.symbol_len as usize]) };
                if s.eq_ignore_ascii_case(symbol) {
                    println!("Tick: {} bid={}", s, bt.bid_price);
                }
            }
            _ => {}
        }
    }
    println!("Stopped");
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    RUNNING.store(false, Ordering::Relaxed);
}