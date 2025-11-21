use crossbeam::channel::Receiver;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, Duration};
use std::ffi::CString;
use std::os::raw::c_char;

static STOP_FLAG: AtomicBool = AtomicBool::new(false);

// ═══════════════════════════════════════════════════════════
// C TYPES
// ═══════════════════════════════════════════════════════════

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CEvent {
    pub event_type: u8,
    pub data: CEventData,
    pub received_at_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union CEventData {
    pub book_ticker: CBookTicker,
    pub trade: CTrade,
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

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OrderResult {
    pub success: bool,
    pub order_id: i64,
    pub error_code: i32,
}

pub type OrderCallback = unsafe extern "C" fn(result: OrderResult);

pub type PlaceOrderFn = unsafe extern "C" fn(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    price: f64,
    quantity: f64,
    side: *const c_char,
    callback: OrderCallback,
);

pub type CancelOrderFn = unsafe extern "C" fn(
    api_key: *const c_char,
    secret_key: *const c_char,
    symbol: *const c_char,
    order_id: i64,
    callback: OrderCallback,
);

// ═══════════════════════════════════════════════════════════
// HELPERS
// ═══════════════════════════════════════════════════════════

impl CBookTicker {
    fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
}

impl CTrade {
    fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
}

// ═══════════════════════════════════════════════════════════
// STRATEGY
// ═══════════════════════════════════════════════════════════

struct Strategy {
    current_bid: f64,
    current_ask: f64,
    events_received: u64,
    orders_sent: u64,
    last_order_time: Instant,
    price_initialized: bool,
}

impl Strategy {
    fn new() -> Self {
        Self {
            current_bid: 0.0,
            current_ask: 0.0,
            events_received: 0,
            orders_sent: 0,
            last_order_time: Instant::now(),
            price_initialized: false,
        }
    }
    
    fn on_book_ticker(&mut self, bt: &CBookTicker) {
        self.events_received += 1;
        
        let symbol = bt.symbol_str();
        
        if symbol.eq_ignore_ascii_case("solusdt") {
            self.current_bid = bt.bid_price;
            self.current_ask = bt.ask_price;
            
            if !self.price_initialized {
                self.price_initialized = true;
                println!("✅ SOLUSDT prices initialized | bid=${:.2} ask=${:.2}", 
                         self.current_bid, self.current_ask);
            }
        }
    }
    
    fn should_place_order(&self) -> bool {
        self.last_order_time.elapsed() >= Duration::from_secs(20)
    }
    
    fn get_order_price(&self) -> f64 {
        self.current_bid * 0.95
    }
    
    fn print_stats(&self) {
        println!("\n═══════════════════════════════════════════");
        println!("📊 STRATEGY STATISTICS");
        println!("═══════════════════════════════════════════");
        println!("Events processed:     {}", self.events_received);
        println!("Orders sent:          {}", self.orders_sent);
        println!("Current bid:          ${:.2}", self.current_bid);
        println!("Current ask:          ${:.2}", self.current_ask);
        println!("═══════════════════════════════════════════\n");
    }
}

// ═══════════════════════════════════════════════════════════
// ORDER CALLBACK
// ═══════════════════════════════════════════════════════════

unsafe extern "C" fn order_result_handler(result: OrderResult) {
    if result.success {
        println!("✅ Order placed | ID: {}", result.order_id);
    } else {
        println!("❌ Order failed | Error code: {}", result.error_code);
    }
}

// ═══════════════════════════════════════════════════════════
// MAIN RUN FUNCTION
// ═══════════════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
) -> i32 {
    println!("🚀 SOL Auto Buyer started | 0.1 SOL every 20s @ bid -5%");
    
    let rx = unsafe { &*rx_ptr };
    let mut strategy = Strategy::new();
    
    let api_key = CString::new("ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm").unwrap();
    let secret_key = CString::new("3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4").unwrap();
    let symbol = CString::new("SOLUSDT").unwrap();
    let side = CString::new("BUY").unwrap();
    let quantity = 0.1;
    
    while !STOP_FLAG.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                match event.event_type {
                    0 => {
                        let bt = unsafe { &event.data.book_ticker };
                        strategy.on_book_ticker(bt);
                        
                        if strategy.should_place_order() && strategy.current_bid > 0.0 {
                            let order_price = strategy.get_order_price();
                            
                            println!("\n📝 Placing order #{} | {} SOL @ ${:.2} (bid ${:.2} -5%)",
                                     strategy.orders_sent + 1,
                                     quantity,
                                     order_price,
                                     strategy.current_bid);
                            
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
                            strategy.last_order_time = Instant::now();
                        }
                    }
                    1 => {} // Trade events - ignore
                    _ => {}
                }
            }
            Err(crossbeam::channel::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    
    strategy.print_stats();
    println!("🛑 Strategy stopped");
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    println!("🛑 Stop signal received");
    STOP_FLAG.store(true, Ordering::Relaxed);
}