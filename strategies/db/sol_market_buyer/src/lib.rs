mod types;
use types::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::ffi::CString;

use crossbeam::channel::Receiver;
use serde::Deserialize;

static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

// ХАРДКОД API-ключей (ЗАМЕНИ НА СВОИ)
const API_KEY: &str = "ay0LdRfUbErVi6jTAanIjiuASqId3oLQYibIwCRQY3OLKiKdHCVGvLQgtH9X5wWm";
const SECRET_KEY: &str = "3AJ2HfjBkNPKwfsPswkzAkqVx6Jawm3xSQTOMKJ1ib5e4Wa9DEM5ddvfhDjeqPO4";

#[derive(Debug, Clone, Deserialize)]
struct StrategyParams {
    #[serde(default = "default_order_qty")]
    order_qty: f64,
    #[serde(default = "default_interval_ms")]
    interval_ms: u64,
}

fn default_order_qty() -> f64 { 0.1 }
fn default_interval_ms() -> u64 { 10_000 }

impl Default for StrategyParams {
    fn default() -> Self {
        Self {
            order_qty: default_order_qty(),
            interval_ms: default_interval_ms(),
        }
    }
}

unsafe extern "C" fn on_order_placed(result: OrderResult) {
    if result.success {
        println!("✅ Market BUY placed, order_id={}", result.order_id);
    } else {
        println!("❌ Market BUY failed, error_code={}", result.error_code);
    }
}

#[no_mangle]
pub unsafe extern "C" fn run(
    rx: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32 {
    SHOULD_STOP.store(false, Ordering::Relaxed);

    let rx = &*rx;
    let symbol = config.symbol_str().to_string();

    let params: StrategyParams = config
        .parse_params()
        .unwrap_or_else(|e| {
            println!("⚠️ Failed to parse params_json: {e}, using defaults");
            StrategyParams::default()
        });

    println!("🚀 Periodic market buyer started");
    println!("   Symbol: {}", symbol);
    println!("   Params: {:?}", params);

    let interval = Duration::from_millis(params.interval_ms);
    let mut last_order_time = Instant::now();

    while !SHOULD_STOP.load(Ordering::Relaxed) {
        let _ = rx.recv_timeout(Duration::from_millis(100));

        if last_order_time.elapsed() >= interval {
            last_order_time = Instant::now();

            if params.order_qty <= 0.0 {
                println!("⚠️ order_qty <= 0, skip order");
                continue;
            }

            let api_key = CString::new(API_KEY).unwrap();
            let secret_key = CString::new(SECRET_KEY).unwrap();
            let symbol_c = CString::new(symbol.as_str()).unwrap();
            let side = CString::new("BUY").unwrap();

            let qty = params.order_qty;
            let price = 0.0_f64;     // для MARKET игнорируется
            let order_type: u8 = 1;  // 0 = LIMIT, 1 = MARKET

            println!("📥 Sending MARKET BUY {} {}", qty, symbol);

            place_order(
                api_key.as_ptr(),
                secret_key.as_ptr(),
                symbol_c.as_ptr(),
                price,
                qty,
                side.as_ptr(),
                order_type,
                on_order_placed,
            );
        }
    }

    println!("✅ Periodic market buyer stopped");
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    println!("🛑 Stop signal received");
    SHOULD_STOP.store(true, Ordering::Relaxed);
}
