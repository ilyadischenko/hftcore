// strategies/db/funding_catcher/src/lib.rs

mod types;
use types::*;

use crossbeam::channel::Receiver;
use serde::Deserialize;
use std::ffi::CString;
use std::time::Duration;
use chrono::{Local, TimeZone, Duration as ChronoDuration};

// ═══════════════════════════════════════════════════════════
// ПАРАМЕТРЫ
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Deserialize)]
struct EntryPoint {
    seconds_before: u64,
    quantity: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct StrategyParams {
    entries: Vec<EntryPoint>,
    target_hour: u8,
    target_minute: u8,
    #[serde(default = "default_exit_delay_ms")]
    exit_delay_ms: u64,
    #[serde(default)]
    repeat: bool,
    api_key: String,
    secret_key: String,
}

fn default_exit_delay_ms() -> u64 { 100 }

// ═══════════════════════════════════════════════════════════
// CALLBACKS
// ═══════════════════════════════════════════════════════════

unsafe extern "C" fn on_entry(result: OrderResult) {
    if result.success {
        println!("   ✅ Entry filled #{}", result.order_id);
    } else {
        println!("   ❌ Entry failed: {}", result.error_code);
    }
}

unsafe extern "C" fn on_exit(result: OrderResult) {
    if result.success {
        println!("   ✅ Exit filled #{}", result.order_id);
    } else {
        println!("   ❌ Exit failed: {}", result.error_code);
    }
}

// ═══════════════════════════════════════════════════════════
// SCHEDULE
// ═══════════════════════════════════════════════════════════

struct ScheduledEntry {
    time: chrono::DateTime<Local>,
    quantity: f64,
    executed: bool,
}

fn build_schedule(
    now: chrono::DateTime<Local>,
    hour: u8,
    minute: u8,
    entries: &[EntryPoint],
    exit_delay_ms: u64,
) -> (Vec<ScheduledEntry>, chrono::DateTime<Local>, chrono::DateTime<Local>) {
    let today = now.date_naive();
    let target_naive = today
        .and_hms_opt(hour as u32, minute as u32, 0)
        .unwrap();

    let mut funding_time = Local.from_local_datetime(&target_naive).unwrap();

    // Если самый ранний вход уже прошёл - следующий день
    let max_before = entries.iter().map(|e| e.seconds_before).max().unwrap_or(0);
    let earliest = funding_time - ChronoDuration::seconds(max_before as i64);
    
    if earliest <= now {
        let tomorrow = today + ChronoDuration::days(1);
        let target_naive = tomorrow.and_hms_opt(hour as u32, minute as u32, 0).unwrap();
        funding_time = Local.from_local_datetime(&target_naive).unwrap();
    }

    let exit_time = funding_time + ChronoDuration::milliseconds(exit_delay_ms as i64);

    let mut schedule: Vec<ScheduledEntry> = entries.iter()
        .map(|e| ScheduledEntry {
            time: funding_time - ChronoDuration::seconds(e.seconds_before as i64),
            quantity: e.quantity,
            executed: false,
        })
        .collect();

    // Сортируем по времени
    schedule.sort_by_key(|s| s.time);

    (schedule, funding_time, exit_time)
}

// ═══════════════════════════════════════════════════════════
// MAIN
// ═══════════════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32 {
    if rx_ptr.is_null() {
        eprintln!("❌ rx_ptr is null");
        return -1;
    }

    let rx = unsafe { &*rx_ptr };
    let symbol = config.symbol().to_string();

    // Парсим параметры
    let params: StrategyParams = match serde_json::from_str(config.params_raw()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("❌ Invalid params: {}", e);
            return -2;
        }
    };

    // Валидация
    if params.entries.is_empty() {
        eprintln!("❌ No entry points");
        return -3;
    }
    if params.api_key.is_empty() || params.secret_key.is_empty() {
        eprintln!("❌ Missing credentials");
        return -4;
    }

    // CStrings живут до конца run()
    let api_key_c = CString::new(params.api_key.as_str()).unwrap();
    let secret_key_c = CString::new(params.secret_key.as_str()).unwrap();
    let symbol_c = CString::new(symbol.as_str()).unwrap();
    let buy_c = CString::new("BUY").unwrap();
    let sell_c = CString::new("SELL").unwrap();

    let api_key_ptr = api_key_c.as_ptr();
    let secret_key_ptr = secret_key_c.as_ptr();
    let symbol_ptr = symbol_c.as_ptr();
    let buy_ptr = buy_c.as_ptr();
    let sell_ptr = sell_c.as_ptr();

    println!("🚀 FundingCatcher | {} | {:02}:{:02}", symbol, params.target_hour, params.target_minute);
    println!("   Entries: {:?}", params.entries.iter()
        .map(|e| format!("-{}s: {}", e.seconds_before, e.quantity))
        .collect::<Vec<_>>());

    // ═══════════════════════════════════════════════════════════
    // MAIN LOOP
    // ═══════════════════════════════════════════════════════════

    loop {
        if config.should_stop() {
            break;
        }

        let now = Local::now();
        let (mut schedule, funding_time, exit_time) = build_schedule(
            now,
            params.target_hour,
            params.target_minute,
            &params.entries,
            params.exit_delay_ms,
        );

        let total_planned: f64 = schedule.iter().map(|s| s.quantity).sum();
        
        println!("📅 Funding: {} | Exit: {} | Total qty: {}", 
            funding_time.format("%H:%M:%S"),
            exit_time.format("%H:%M:%S%.3f"),
            total_planned
        );

        let mut total_entered = 0.0;
        let mut exit_done = false;

        // Цикл до выхода
        while !config.should_stop() && !exit_done {
            // Drain events
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(_) => {},
                Err(crossbeam::channel::RecvTimeoutError::Timeout) => {},
                Err(crossbeam::channel::RecvTimeoutError::Disconnected) => {
                    println!("⚠️ Channel disconnected");
                    return 0;
                }
            }

            let now = Local::now();

            // ═══════════════════════════════════════════════════════════
            // ENTRIES
            // ═══════════════════════════════════════════════════════════
            for entry in schedule.iter_mut() {
                if !entry.executed && now >= entry.time && now < exit_time {
                    let secs_to_funding = (funding_time - now).num_milliseconds() as f64 / 1000.0;
                    
                    println!("📥 BUY {} {} | {:.1}s to funding", 
                        entry.quantity, symbol, secs_to_funding);
                    
                    unsafe {
                        place_order(
                            api_key_ptr,
                            secret_key_ptr,
                            symbol_ptr,
                            0.0,
                            entry.quantity,
                            buy_ptr,
                            1, // MARKET
                            on_entry,
                        );
                    }
                    
                    total_entered += entry.quantity;
                    entry.executed = true;
                }
            }

            // ═══════════════════════════════════════════════════════════
            // EXIT
            // ═══════════════════════════════════════════════════════════
            if total_entered > 0.0 && now >= exit_time {
                let ms_after = (now - funding_time).num_milliseconds();
                
                println!("📤 SELL {} {} | +{}ms after funding", 
                    total_entered, symbol, ms_after);
                
                unsafe {
                    place_order(
                        api_key_ptr,
                        secret_key_ptr,
                        symbol_ptr,
                        0.0,
                        total_entered,
                        sell_ptr,
                        1, // MARKET
                        on_exit,
                    );
                }
                
                // Ждём callback
                std::thread::sleep(Duration::from_millis(500));
                exit_done = true;
            }
        }

        if config.should_stop() {
            break;
        }

        if !params.repeat {
            println!("✅ Cycle complete");
            break;
        }

        println!("🔁 Waiting for next cycle...\n");
    }

    println!("🛑 FundingCatcher stopped");
    0
}