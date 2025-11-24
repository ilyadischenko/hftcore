mod types;
use types::*;

use crossbeam::channel::Receiver;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::ffi::CString;

static STOP_FLAG: AtomicBool = AtomicBool::new(false);

struct Strategy {
    last_bid: f64,
    changes_count: u64,
    orders_sent: u64,
}

impl Strategy {
    fn new() -> Self {
        Self {
            last_bid: 0.0,
            changes_count: 0,
            orders_sent: 0,
        }
    }
    
    fn on_book_ticker(&mut self, bt: &CBookTicker) -> bool {
        let symbol = bt.symbol_str();
        
        if !symbol.eq_ignore_ascii_case("solusdt") {
            return false;
        }
        
        if bt.bid_price != self.last_bid && self.last_bid != 0.0 {
            self.changes_count += 1;
            
            if self.changes_count % 20 == 0 {
                println!("✅ 100 changes! Total: {} | Bid: ${:.2}", 
                         self.changes_count, bt.bid_price);
                self.last_bid = bt.bid_price;
                return true;
            }
        }
        
        self.last_bid = bt.bid_price;
        false
    }
    
    fn get_order_price(&self) -> f64 {
        let price = self.last_bid - 1.0;
        (price * 100.0).round() / 100.0
    }
}

unsafe extern "C" fn order_result_handler(result: OrderResult) {
    if result.success {
        println!("✅ Order ID: {}", result.order_id);
    } else {
        println!("❌ Error: {}", result.error_code);
    }
}

#[no_mangle]
pub extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
) -> i32 {
    println!("🚀 SOL Bid Tracker | Order every 100 changes @ bid-$1");
    
    let rx = unsafe { &*rx_ptr };
    let mut strategy = Strategy::new();
    
    let api_key = CString::new("ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm").unwrap();
    let secret_key = CString::new("3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4").unwrap();
    let symbol = CString::new("SOLUSDT").unwrap();
    let side = CString::new("BUY").unwrap();
    let quantity = 1.0;
    
    while !STOP_FLAG.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                if event.event_type == 0 {
                    let bt = unsafe { &event.data.book_ticker };
                    
                    if strategy.on_book_ticker(bt) {
                        let order_price = strategy.get_order_price();
                        
                        println!("\n📝 Order #{} | {} SOL @ ${:.2} (bid ${:.2} - $1)",
                                 strategy.orders_sent + 1,
                                 quantity,
                                 order_price,
                                 strategy.last_bid);
                        
                        unsafe {
                            place_order(
                                api_key.as_ptr(),
                                secret_key.as_ptr(),
                                symbol.as_ptr(),
                                order_price,
                                quantity,
                                side.as_ptr(),
                                order_result_handler,
                            );
                        }
                        
                        strategy.orders_sent += 1;
                    }
                }
            }
            Err(crossbeam::channel::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    
    println!("\n🛑 Strategy stopped");
    println!("   Total bid changes: {}", strategy.changes_count);
    println!("   Orders sent: {}", strategy.orders_sent);
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    println!("🛑 Stop signal received");
    STOP_FLAG.store(true, Ordering::Relaxed);
}