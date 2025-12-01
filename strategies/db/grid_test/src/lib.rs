mod types;
use types::*;

use types::*;

use crossbeam::channel::Receiver;
use std::time::Duration;
use std::ffi::CString;
use serde::Deserialize;

// ═══════════════════════════════════════════════════════════
// КОНФИГУРАЦИЯ
// ═══════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct Params {
    api_key: String,
    secret_key: String,
    
    start_price: f64,      // Цена первого ордера
    qty_per_layer: f64,    // Объем (в монетах) на один ордер
    layers_count: usize,   // Количество ордеров в сетке
    step_pct: f64,         // Шаг сетки в % (например 0.2)
    
    tick_size: f64,        // Шаг цены инструмента (например 0.1)
    step_size: f64,        // Шаг объема инструмента (например 0.001)
}

// ═══════════════════════════════════════════════════════════
// ЛОГИКА
// ═══════════════════════════════════════════════════════════

fn round_step(value: f64, step: f64) -> f64 {
    let factor = 1.0 / step;
    (value * factor).round() / factor
}

#[no_mangle]
pub unsafe extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place: PlaceOrderFn,
    _cancel: CancelOrderFn,
    config: StrategyConfig
) -> i32 {
    let rx = &*rx_ptr;
    
    // 1. Парсим параметры
    let params: Params = match serde_json::from_str(config.params_str()) {
        Ok(p) => p,
        Err(e) => {
            println!("❌ Config parse error: {}", e);
            return -1;
        }
    };
    
    let symbol = config.symbol();
    
    // Подготовка C-строк для FFI
    let c_api = CString::new(params.api_key.clone()).unwrap();
    let c_sec = CString::new(params.secret_key.clone()).unwrap();
    let c_sym = CString::new(symbol).unwrap();
    let c_side = CString::new("BUY").unwrap(); // Только покупки

    println!("🚀 STARTING GRID (Standard GTC): {} layers, Step {}%", 
             params.layers_count, params.step_pct);

    // 2. Выставляем сетку СРАЗУ
    for i in 0..params.layers_count {
        // Расчет цены: Start * (1 - (i * step%))
        let step_multiplier = params.step_pct / 100.0;
        let raw_price = params.start_price * (1.0 - (i as f64 * step_multiplier));
        
        let price = round_step(raw_price, params.tick_size);
        let qty = round_step(params.qty_per_layer, params.step_size);

        println!("   👉 Placing Layer #{}: Price {:.2} | Qty {:.3}", i+1, price, qty);

        place(
            c_api.as_ptr(),
            c_sec.as_ptr(),
            c_sym.as_ptr(),
            price,
            qty,
            c_side.as_ptr(),
            0,     // 0 = LIMIT Order
            false, // reduce_only = false
            on_res
        );
    }

    println!("✅ All orders sent. Waiting for updates...");

    // 3. Loop для поддержания жизни стратегии
    loop {
        if config.should_stop() { 
            println!("🛑 Strategy stopping...");
            break; 
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                if let Some(o) = event.as_order() {
                     if o.status_char == b'F' { // FILLED
                         println!("💰 Order {} FILLED!", o.order_id);
                    }
                }
            },
            Err(_) => continue,
        }
    }

    0
}

unsafe extern "C" fn on_res(res: OrderResult) {
    if !res.success {
        println!("❌ Order Error: Code {}", res.error_code);
    }
}