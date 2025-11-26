mod types;
use types::*;

use crossbeam::channel::Receiver;
use serde::Deserialize;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{Local, TimeZone, Duration as ChronoDuration};

static STOP_FLAG: AtomicBool = AtomicBool::new(false);

// Статическое хранилище для строк - живут всё время работы программы
static API_KEY_C: OnceLock<CString> = OnceLock::new();
static SECRET_KEY_C: OnceLock<CString> = OnceLock::new();
static SYMBOL_C: OnceLock<CString> = OnceLock::new();
static BUY_SIDE_C: OnceLock<CString> = OnceLock::new();
static SELL_SIDE_C: OnceLock<CString> = OnceLock::new();

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
    println!("🔔 on_entry_placed CALLBACK STARTED");
    if result.success {
        println!("✅ ENTRY order placed, id={}", result.order_id);
    } else {
        println!("❌ ENTRY order failed, code={}", result.error_code);
    }
    println!("🔔 on_entry_placed CALLBACK FINISHED");
}

unsafe extern "C" fn on_exit_placed(result: OrderResult) {
    println!("🔔 on_exit_placed CALLBACK STARTED");
    if result.success {
        println!("✅ EXIT order placed, id={}", result.order_id);
    } else {
        println!("❌ EXIT order failed, code={}", result.error_code);
    }
    println!("🔔 on_exit_placed CALLBACK FINISHED");
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

/// Инициализирует статические CString'и
/// Возвращает true если успешно, false если ошибка
fn init_static_strings(api_key: &str, secret_key: &str, symbol: &str) -> bool {
    // Инициализируем только один раз
    let api_result = API_KEY_C.get_or_init(|| {
        CString::new(api_key).unwrap_or_else(|_| CString::new("").unwrap())
    });
    
    let secret_result = SECRET_KEY_C.get_or_init(|| {
        CString::new(secret_key).unwrap_or_else(|_| CString::new("").unwrap())
    });
    
    let symbol_result = SYMBOL_C.get_or_init(|| {
        CString::new(symbol).unwrap_or_else(|_| CString::new("").unwrap())
    });
    
    BUY_SIDE_C.get_or_init(|| CString::new("BUY").unwrap());
    SELL_SIDE_C.get_or_init(|| CString::new("SELL").unwrap());
    
    // Проверяем что строки не пустые (если входные данные были непустыми)
    if !api_key.is_empty() && api_result.as_bytes().is_empty() {
        return false;
    }
    if !secret_key.is_empty() && secret_result.as_bytes().is_empty() {
        return false;
    }
    if !symbol.is_empty() && symbol_result.as_bytes().is_empty() {
        return false;
    }
    
    true
}

#[no_mangle]
pub extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32 {
    STOP_FLAG.store(false, Ordering::Relaxed);

    // Проверка указателя
    if rx_ptr.is_null() {
        println!("❌ ERROR: rx_ptr is null!");
        return -1;
    }

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

    // Инициализируем статические строки
    if !init_static_strings(&params.api_key, &params.secret_key, &symbol) {
        println!("❌ ERROR: Failed to initialize static strings!");
        return -2;
    }

    // Получаем указатели на статические строки - они гарантированно живут
    let api_key_ptr = API_KEY_C.get().unwrap().as_ptr();
    let secret_key_ptr = SECRET_KEY_C.get().unwrap().as_ptr();
    let symbol_ptr = SYMBOL_C.get().unwrap().as_ptr();
    let buy_side_ptr = BUY_SIDE_C.get().unwrap().as_ptr();
    let sell_side_ptr = SELL_SIDE_C.get().unwrap().as_ptr();

    println!("✅ Static strings initialized successfully");

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
            Ok(event) => {
                // Можно добавить обработку событий если нужно
                println!("📨 Received event");
            }
            Err(crossbeam::channel::RecvTimeoutError::Timeout) => {
                // Нормальный таймаут, продолжаем
            }
            Err(crossbeam::channel::RecvTimeoutError::Disconnected) => {
                println!("⚠️ Event channel disconnected");
                break;
            }
        }

        now = Local::now();

        // ENTRY логика
        if !entry_sent && now >= entry_time && now < exit_time {
            println!("⏰ ENTRY time reached: {}", now.format("%Y-%m-%d %H:%M:%S%.3f"));
            
            if params.order_qty > 0.0 
                && !params.api_key.is_empty() 
                && !params.secret_key.is_empty() 
            {
                println!(
                    "📥 ENTRY: sending MARKET BUY {} {} at {}",
                    params.order_qty,
                    symbol,
                    now.format("%Y-%m-%d %H:%M:%S%.3f"),
                );
                
                println!("🔄 Calling place_order for ENTRY...");
                unsafe {
                    place_order(
                        api_key_ptr,
                        secret_key_ptr,
                        symbol_ptr,
                        0.0,
                        params.order_qty,
                        buy_side_ptr,
                        1,
                        on_entry_placed,
                    );
                }
                println!("✅ place_order for ENTRY returned");
                
                entry_sent = true;
            } else {
                println!("⚠️ ENTRY conditions not met, skipping");
                entry_sent = true;
            }
        }

        // EXIT логика
        if entry_sent && !exit_sent && now >= exit_time {
            println!("⏰ EXIT time reached: {}", now.format("%Y-%m-%d %H:%M:%S%.3f"));
            
            if params.order_qty > 0.0 
                && !params.api_key.is_empty() 
                && !params.secret_key.is_empty() 
            {
                println!(
                    "📤 EXIT: sending MARKET SELL {} {} at {}",
                    params.order_qty,
                    symbol,
                    now.format("%Y-%m-%d %H:%M:%S%.3f"),
                );
                
                println!("🔄 Calling place_order for EXIT...");
                unsafe {
                    place_order(
                        api_key_ptr,
                        secret_key_ptr,
                        symbol_ptr,
                        0.0,
                        params.order_qty,
                        sell_side_ptr,
                        1,
                        on_exit_placed,
                    );
                }
                println!("✅ place_order for EXIT returned");
                
                exit_sent = true;
            } else {
                println!("⚠️ EXIT conditions not met, skipping");
                exit_sent = true;
            }

            // Ждём чтобы callback успел выполниться
            println!("⏳ Waiting for callback to complete...");
            std::thread::sleep(Duration::from_millis(500));

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
                println!("✅ One-shot mode: waiting before exit...");
                // Даём время на завершение всех асинхронных операций
                std::thread::sleep(Duration::from_secs(2));
                println!("✅ One-shot mode: finished after first funding cycle");
                break;
            }
        }
    }

    // Финальное ожидание для асинхронных callback'ов
    println!("⏳ Final wait for pending callbacks...");
    std::thread::sleep(Duration::from_secs(1));
    
    println!("🛑 Funding Collector stopped");
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    println!("🛑 Stop signal received");
    STOP_FLAG.store(true, Ordering::Relaxed);
}