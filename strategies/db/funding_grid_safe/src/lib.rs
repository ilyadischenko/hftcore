mod types;
use types::*;

use crossbeam::channel::{Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime};
use chrono::{DateTime, Utc, Timelike};
use std::ffi::CString;
use serde::Deserialize;

#[derive(Deserialize)]
struct Params {
    api_key: String,
    secret_key: String,
    target_hour: u32,
    
    // Grid Settings (Lots)
    grid_layers: usize,
    base_qty: f64,         
    qty_increment: f64,    
    
    // Price Settings
    initial_offset_pct: f64,
    step_pct: f64,
    
    // Precision
    tick_size: f64,
    step_size: f64,
    
    // Timing
    trigger_ms_before: u32,
    panic_seconds: u32,
}

#[derive(PartialEq)]
enum State {
    Idle,
    GridPlaced,
    Done,
}

struct Context {
    state: State,
    open_orders: Vec<i64>,
    max_possible_pos: f64, // Сумма всех ведер
}

fn round_step(value: f64, step: f64) -> f64 {
    let factor = 1.0 / step;
    (value * factor).round() / factor
}

#[no_mangle]
pub unsafe extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place: PlaceOrderFn,
    cancel: CancelOrderFn,
    config: StrategyConfig
) -> i32 {
    let rx = &*rx_ptr;
    let params: Params = serde_json::from_str(config.params_str()).unwrap();
    let symbol = config.symbol();
    
    let c_api = CString::new(params.api_key.clone()).unwrap();
    let c_sec = CString::new(params.secret_key.clone()).unwrap();
    let c_sym = CString::new(symbol).unwrap();
    let c_buy = CString::new("BUY").unwrap();
    let c_sell = CString::new("SELL").unwrap();

    let mut ctx = Context {
        state: State::Idle,
        open_orders: Vec::new(),
        max_possible_pos: 0.0,
    };

    println!("🛡 [SAFE GRID] {} | Layers: {} | Waiting for trigger...", symbol, params.grid_layers);

    loop {
        if config.should_stop() { break; }

        let now = SystemTime::now();
        let dt: DateTime<Utc> = now.into();
        let h = dt.hour();
        let m = dt.minute();
        let s = dt.second();
        let ms = dt.timestamp_subsec_millis();

        // PANIC EXIT
        if ctx.state == State::GridPlaced && h == params.target_hour && s >= params.panic_seconds {
            println!("🚨 [PANIC] Timeout!");
            exit_all(&mut ctx, &c_api, &c_sec, &c_sym, &c_sell, cancel, place);
            ctx.state = State::Done;
        }

        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(event) => {
                match event.event_type {
                    // TRIGGER (Market Data)
                    0 => {
                        if ctx.state == State::Idle {
                            let target_prev = if params.target_hour == 0 { 23 } else { params.target_hour - 1 };
                            if h == target_prev && m == 59 && s == 59 && ms >= (1000 - params.trigger_ms_before) {
                                if let Some(bt) = event.as_book_ticker() {
                                    let anchor = bt.bid_price;
                                    println!("⚡ TRIGGER! Anchor: {}", anchor);
                                    
                                    for i in 0..params.grid_layers {
                                        // Price
                                        let pct_drop = params.initial_offset_pct + (i as f64 * params.step_pct);
                                        let raw_price = anchor * (1.0 - (pct_drop / 100.0));
                                        let price = round_step(raw_price, params.tick_size);
                                        
                                        // Qty
                                        let raw_qty = params.base_qty + (i as f64 * params.qty_increment);
                                        let qty = round_step(raw_qty, params.step_size);
                                        
                                        // СУММИРУЕМ ОБЪЕМ ДЛЯ БЕЗОПАСНОГО ВЫХОДА
                                        ctx.max_possible_pos += qty;
                                        
                                        println!("   ➡️ Layer {}: {} @ {}", i+1, qty, price);
                                        
                                        place(
                                            c_api.as_ptr(), c_sec.as_ptr(), c_sym.as_ptr(),
                                            price, qty, c_buy.as_ptr(), 0, 
                                            false, 
                                            on_res
                                        );
                                    }
                                    println!("🛡 Max potential position: {}", ctx.max_possible_pos);
                                    ctx.state = State::GridPlaced;
                                }
                            }
                        }
                    },
                    // ORDER UPDATE (Collect IDs)
                    2 => {
                        if let Some(o) = event.as_order() {
                            if o.side_char == b'B' && o.status_char == b'N' {
                                ctx.open_orders.push(o.order_id);
                            }
                        }
                    },
                    // FUNDING EXIT
                    3 => {
                         if ctx.state == State::GridPlaced {
                             if let Some(_) = event.as_funding() {
                                 println!("💸 FUNDING! EXITING!");
                                 exit_all(&mut ctx, &c_api, &c_sec, &c_sym, &c_sell, cancel, place);
                                 ctx.state = State::Done;
                             }
                         }
                    },
                    _ => {}
                }
            },
            Err(_) => continue,
        }
    }
    0
}

unsafe fn exit_all(
    ctx: &mut Context,
    api: &CString, sec: &CString, sym: &CString, side: &CString,
    cancel: CancelOrderFn, place: PlaceOrderFn
) {
    // 1. Отмена ведер
    for id in &ctx.open_orders {
        cancel(api.as_ptr(), sec.as_ptr(), sym.as_ptr(), *id, on_res);
    }
    
    // 2. Расчет безопасного объема выхода
    // Умножаем на 2.0 для гарантии, но это число разумное
    let exit_qty = if ctx.max_possible_pos > 0.0 { 
        ctx.max_possible_pos * 2.0 
    } else { 
        10000.0 // Фолбэк на случай глюка, но не миллион
    };
    
    println!("🏃 REDUCE ONLY SELL: {}", exit_qty);
    
    place(
        api.as_ptr(), sec.as_ptr(), sym.as_ptr(),
        0.0, exit_qty, side.as_ptr(), 1, // Market
        true, // ReduceOnly
        on_res
    );
}

unsafe extern "C" fn on_res(_: OrderResult) {}