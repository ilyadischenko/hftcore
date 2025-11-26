mod types;
use types::*;

use crossbeam::channel::Receiver;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::ffi::CString;
use serde::Deserialize;

static STOP_FLAG: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize)]
struct Params {
    changes_trigger: u64,
    order_size: f64,
    price_offset_percent: f64,
    #[serde(default = "default_max_orders")]
    max_orders: u64,
    #[serde(default)]
    enable_logging: bool,
}

fn default_max_orders() -> u64 { 0 }

impl Default for Params {
    fn default() -> Self {
        Self {
            changes_trigger: 10,
            order_size: 0.1,
            price_offset_percent: -1.0,
            max_orders: 1,
            enable_logging: false,
        }
    }
}

struct Strategy {
    symbol: String,
    params: Params,
    last_bid: f64,
    changes_count: u64,
    orders_sent: u64,
}

impl Strategy {
    fn new(config: &StrategyConfig) -> Self {
        let symbol = config.symbol_str().to_string();
        let params: Params = config.parse_params().unwrap_or_else(|e| {
            println!("⚠️ Failed to parse params: {}, using defaults", e);
            Params::default()
        });
        println!("\n╔════════════════════════════════════════════╗");
        println!("║   🎯 BID CHANGE TRACKER INITIALIZED       ║");
        println!("╚════════════════════════════════════════════╝");
        println!("Symbol:            {}", symbol);
        println!("Changes trigger:   {} bid changes", params.changes_trigger);
        println!("Order size:        {} {}", params.order_size, symbol.trim_end_matches("USDT"));
        println!("Price offset:      {}% from bid", params.price_offset_percent);
        println!("Max orders:        {}", if params.max_orders == 0 { "unlimited".to_string() } else { params.max_orders.to_string() });
        println!("Logging:           {}", params.enable_logging);
        println!("════════════════════════════════════════════\n");
        Self { symbol, params, last_bid: 0.0, changes_count: 0, orders_sent: 0 }
    }
    fn on_book_ticker(&mut self, bt: &CBookTicker) -> Option<f64> {
        if !bt.symbol_str().eq_ignore_ascii_case(&self.symbol) { return None; }
        if bt.bid_price != self.last_bid && self.last_bid != 0.0 {
            self.changes_count += 1;
            if self.params.enable_logging && self.changes_count % 10 == 0 {
                println!("📊 Changes: {} | Bid: ${:.2}", self.changes_count, bt.bid_price);
            }
            if self.changes_count % self.params.changes_trigger == 0 {
                if self.params.max_orders > 0 && self.orders_sent >= self.params.max_orders {
                    println!("⚠️ Max orders limit reached: {}/{}", self.orders_sent, self.params.max_orders);
                    return None;
                }
                self.last_bid = bt.bid_price;
                let order_price = self.calculate_order_price(bt.bid_price);
                println!("\n✅ Trigger hit! {} changes reached", self.params.changes_trigger);
                println!("   Total changes: {}", self.changes_count);
                println!("   Current bid:   ${:.2}", bt.bid_price);
                return Some(order_price);
            }
        }
        self.last_bid = bt.bid_price;
        None
    }
    fn calculate_order_price(&self, current_bid: f64) -> f64 {
        let price = current_bid * (1.0 + self.params.price_offset_percent / 100.0);
        (price * 100.0).round() / 100.0
    }
    fn print_stats(&self) {
        println!("\n╔════════════════════════════════════════════╗");
        println!("║   📊 STRATEGY STATISTICS                  ║");
        println!("╚════════════════════════════════════════════╝");
        println!("Symbol:           {}", self.symbol);
        println!("Total changes:    {}", self.changes_count);
        println!("Orders sent:      {}", self.orders_sent);
        println!("Last bid:         ${:.2}", self.last_bid);
        if self.orders_sent > 0 {
            println!("Avg changes/order: {:.1}", self.changes_count as f64 / self.orders_sent as f64);
        }
        println!("════════════════════════════════════════════\n");
    }
}

unsafe extern "C" fn order_callback(result: OrderResult) {
    if result.success {
        println!("   ✅ Order placed successfully | ID: {}", result.order_id);
    } else {
        println!("   ❌ Order failed | Error code: {}", result.error_code);
    }
}

#[no_mangle]
pub extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32 {
    println!("🚀 Bid Change Tracker started");
    let rx = unsafe { &*rx_ptr };
    let mut strategy = Strategy::new(&config);
    let api_key = CString::new("ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm").expect("Invalid API key");
    let secret_key = CString::new("3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4").expect("Invalid secret key");
    let symbol = CString::new(strategy.symbol.clone()).expect("Invalid symbol");
    let side = CString::new("BUY").expect("Invalid side");
    println!("📡 Listening for {} events...\n", strategy.symbol);
    while !STOP_FLAG.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) if event.event_type == 0 => {
                let bt = unsafe { &event.data.book_ticker };
                if let Some(order_price) = strategy.on_book_ticker(bt) {
                    println!("\n📝 Placing order #{}", strategy.orders_sent + 1);
                    println!("   Symbol:   {}", strategy.symbol);
                    println!("   Side:     BUY");
                    println!("   Price:    ${:.2} ({}% from bid)", order_price, strategy.params.price_offset_percent);
                    println!("   Quantity: {}", strategy.params.order_size);
                    unsafe {
                        place_order(
                            api_key.as_ptr(),
                            secret_key.as_ptr(),
                            symbol.as_ptr(),
                            order_price,
                            strategy.params.order_size,
                            side.as_ptr(),
                            1,
                            order_callback,
                        );
                    }
                    strategy.orders_sent += 1;
                }
            }
            Ok(_) => {}
            Err(crossbeam::channel::RecvTimeoutError::Timeout) => continue,
            Err(_) => {
                println!("⚠️ Event channel closed");
                break;
            }
        }
    }
    strategy.print_stats();
    println!("🛑 Strategy stopped");
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    println!("\n🛑 Stop signal received");
    STOP_FLAG.store(true, Ordering::Relaxed);
}