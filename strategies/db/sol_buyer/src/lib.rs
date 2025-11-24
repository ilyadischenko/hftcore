use crossbeam::channel::Receiver;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, Duration};
use std::ffi::CString;
use std::os::raw::c_char;

static STOP_FLAG: AtomicBool = AtomicBool::new(false);

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

struct Strategy {
    orders_sent: u64,
    last_order_time: Instant,
}

impl Strategy {
    fn new() -> Self {
        Self {
            orders_sent: 0,
            last_order_time: Instant::now(),
        }
    }
    
    fn should_place_order(&self) -> bool {
        self.last_order_time.elapsed() >= Duration::from_secs(20)
    }
}

unsafe extern "C" fn order_result_handler(result: OrderResult) {
    if result.success {
        println!("✅ Order placed | ID: {}", result.order_id);
    } else {
        println!("❌ Order failed | Error code: {}", result.error_code);
    }
}

#[no_mangle]
pub extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
) -> i32 {
    println!("🚀 SOL Buyer | 1 SOL @ $127.00 every 20s");
    
    let rx = unsafe { &*rx_ptr };
    let mut strategy = Strategy::new();
    
    let api_key = CString::new("ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm").unwrap();
    let secret_key = CString::new("3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4").unwrap();
    let symbol = CString::new("SOLUSDT").unwrap();
    let side = CString::new("BUY").unwrap();
    
    let price = 127.0;
    let quantity = 1.0;
    
    while !STOP_FLAG.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(_event) => {
                if strategy.should_place_order() {
                    println!("\n📝 Order #{} | {} SOL @ ${:.2}",
                             strategy.orders_sent + 1,
                             quantity,
                             price);
                    
                    unsafe {
                        place_order(
                            api_key.as_ptr(),
                            secret_key.as_ptr(),
                            symbol.as_ptr(),
                            price,
                            quantity,
                            side.as_ptr(),
                            order_result_handler,
                        );
                    }
                    
                    strategy.orders_sent += 1;
                    strategy.last_order_time = Instant::now();
                }
            }
            Err(crossbeam::channel::RecvTimeoutError::Timeout) => {
                if strategy.should_place_order() {
                    println!("\n📝 Order #{} | {} SOL @ ${:.2}",
                             strategy.orders_sent + 1,
                             quantity,
                             price);
                    
                    unsafe {
                        place_order(
                            api_key.as_ptr(),
                            secret_key.as_ptr(),
                            symbol.as_ptr(),
                            price,
                            quantity,
                            side.as_ptr(),
                            order_result_handler,
                        );
                    }
                    
                    strategy.orders_sent += 1;
                    strategy.last_order_time = Instant::now();
                }
            }
            Err(_) => break,
        }
    }
    
    println!("\n🛑 Strategy stopped | Total orders: {}", strategy.orders_sent);
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    println!("🛑 Stop signal received");
    STOP_FLAG.store(true, Ordering::Relaxed);
}