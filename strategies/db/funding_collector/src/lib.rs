mod types;
use types::*;

use crossbeam::channel::Receiver;
use serde::Deserialize;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{Local, TimeZone, Duration as ChronoDuration};

static STOP_FLAG: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Deserialize)]
struct StrategyParams {
    order_qty: f64,
    target_hour: u8,
    target_minute: u8,
    #[serde(default = "default_pre_seconds")]
    pre_seconds: u64,
    #[serde(default = "default_exit_delay_ms")]
    exit_delay_ms: u64,
    #[serde(default)]
    repeat: bool,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    secret_key: String,
}

fn default_pre_seconds() -> u64 { 5 }
fn default_exit_delay_ms() -> u64 { 1 }

impl Default for StrategyParams {
    fn default() -> Self {
        Self {
            order_qty: 0.0,
            target_hour: 0,
            target_minute: 0,
            pre_seconds: default_pre_seconds(),
            exit_delay_ms: default_exit_delay_ms(),
            repeat: false,
            api_key: String::new(),
            secret_key: String::new(),
        }
    }
}

unsafe extern "C" fn on_entry_placed(result: OrderResult) {
    if result.success {
        println!("✅ ENTRY order placed, id={}", result.order_id);
    } else {
        println!("❌ ENTRY order failed, code={}", result.error_code);
    }
}

unsafe extern "C" fn on_exit_placed(result: OrderResult) {
    if result.success {
        println!("✅ EXIT order placed, id={}", result.order_id);
    } else {
        println!("❌ EXIT order failed, code={}", result.error_code);
    }
}

fn compute_next_times(
    now: chrono::DateTime<Local>,
    hour: u8,
    minute: u8,
    pre_seconds: u64,
    exit_delay_ms: u64,
) -> (chrono::DateTime<Local>, chrono::DateTime<Local>) {
    let today = now.date_naive();
    let mut target_naive = today
        .and_hms_opt(hour as u32, minute as u32, 0)
        .unwrap_or_else(|| today.and_hms_opt(0, 0, 0).unwrap());

    let mut target = Local.from_local_datetime(&target_naive).unwrap();

    if target <= now {
        target_naive = (today + ChronoDuration::days(1))
            .and_hms_opt(hour as u32, minute as u32, 0)
            .unwrap();
        target = Local.from_local_datetime(&target_naive).unwrap();
    }

    let entry_time = target - ChronoDuration::seconds(pre_seconds as i64);
    let exit_time = target + ChronoDuration::milliseconds(exit_delay_ms as i64);

    (entry_time, exit_time)
}

#[no_mangle]
pub extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32 {
    STOP_FLAG.store(false, Ordering::Relaxed);

    let rx = unsafe { &*rx_ptr };
    let symbol = config.symbol_str().to_string();

    let params: StrategyParams = config
        .parse_params()
        .unwrap_or_else(|e| {
            println!("⚠️ Failed to parse params: {e}, using defaults");
            StrategyParams::default()
        });

    println!("🚀 Funding Collector started");
    println!("   Symbol: {}", symbol);
    println!("   Params: {:?}", params);

    if params.order_qty <= 0.0 {
        println!("⚠️ order_qty <= 0, strategy will not trade");
    }
    if params.api_key.is_empty() || params.secret_key.is_empty() {
        println!("⚠️ api_key / secret_key are empty, strategy will not place orders");
    }

    let api_key_c = CString::new(params.api_key.clone()).unwrap_or_else(|_| CString::new("").unwrap());
    let secret_key_c = CString::new(params.secret_key.clone()).unwrap_or_else(|_| CString::new("").unwrap());
    let symbol_c = CString::new(symbol.clone()).unwrap();
    let buy_side_c = CString::new("BUY").unwrap();
    let sell_side_c = CString::new("SELL").unwrap();

    let mut now = Local::now();
    let (mut entry_time, mut exit_time) = compute_next_times(
        now,
        params.target_hour,
        params.target_minute,
        params.pre_seconds,
        params.exit_delay_ms,
    );

    println!(
        "🕒 Next funding target at {:02}:{:02}, entry at {}, exit at {}",
        params.target_hour,
        params.target_minute,
        entry_time.format("%Y-%m-%d %H:%M:%S%.3f"),
        exit_time.format("%Y-%m-%d %H:%M:%S%.3f"),
    );

    let mut entry_sent = false;
    let mut exit_sent = false;

    while !STOP_FLAG.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(_) => {}
            Err(crossbeam::channel::RecvTimeoutError::Timeout) => {}
            Err(_) => {
                println!("⚠️ Event channel closed");
                break;
            }
        }

        now = Local::now();

        if !entry_sent && now >= entry_time && now < exit_time {
            if params.order_qty > 0.0 && !params.api_key.is_empty() && !params.secret_key.is_empty() {
                println!(
                    "📥 ENTRY: sending MARKET BUY {} {} at {}",
                    params.order_qty,
                    symbol,
                    now.format("%Y-%m-%d %H:%M:%S%.3f"),
                );
                unsafe {
                    place_order(
                        api_key_c.as_ptr(),
                        secret_key_c.as_ptr(),
                        symbol_c.as_ptr(),
                        0.0,
                        params.order_qty,
                        buy_side_c.as_ptr(),
                        1,
                        on_entry_placed,
                    );
                }
                entry_sent = true;
            } else {
                println!("⚠️ ENTRY conditions not met, skipping");
                entry_sent = true;
            }
        }

        if entry_sent && !exit_sent && now >= exit_time {
            if params.order_qty > 0.0 && !params.api_key.is_empty() && !params.secret_key.is_empty() {
                println!(
                    "📤 EXIT: sending MARKET SELL {} {} at {}",
                    params.order_qty,
                    symbol,
                    now.format("%Y-%m-%d %H:%M:%S%.3f"),
                );
                unsafe {
                    place_order(
                        api_key_c.as_ptr(),
                        secret_key_c.as_ptr(),
                        symbol_c.as_ptr(),
                        0.0,
                        params.order_qty,
                        sell_side_c.as_ptr(),
                        1,
                        on_exit_placed,
                    );
                }
                exit_sent = true;
            } else {
                println!("⚠️ EXIT conditions not met, skipping");
                exit_sent = true;
            }

            if params.repeat {
                now = Local::now();
                let (new_entry, new_exit) = compute_next_times(
                    now,
                    params.target_hour,
                    params.target_minute,
                    params.pre_seconds,
                    params.exit_delay_ms,
                );
                entry_time = new_entry;
                exit_time = new_exit;
                entry_sent = false;
                exit_sent = false;

                println!(
                    "🔁 Next cycle: entry at {}, exit at {}",
                    entry_time.format("%Y-%m-%d %H:%M:%S%.3f"),
                    exit_time.format("%Y-%m-%d %H:%M:%S%.3f"),
                );
            } else {
                println!("✅ One-shot mode: finished after first funding cycle");
                break;
            }
        }
    }

    println!("🛑 Funding Collector stopped");
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    println!("🛑 Stop signal received");
    STOP_FLAG.store(true, Ordering::Relaxed);
}
