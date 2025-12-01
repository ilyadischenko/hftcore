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
    target_minute: u32,
    
    grid_layers: usize,
    base_qty: f64,
    qty_increment: f64,
    initial_offset_pct: f64,
    step_pct: f64,
    tick_size: f64,
    step_size: f64,
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
    layer_qtys: Vec<f64>,
    total_exit_qty: f64,
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
    let params: Params = match serde_json::from_str(config.params_str()) {
        Ok(p) => p,
        Err(e) => {
             println!("❌ Config Error: {}", e);
             return -1;
        }
    };
    
    let symbol = config.symbol();
    let c_api = CString::new(params.api_key.clone()).unwrap();
    let c_sec = CString::new(params.secret_key.clone()).unwrap();
    let c_sym = CString::new(symbol).unwrap();
    let c_buy = CString::new("BUY").unwrap();
    let c_sell = CString::new("SELL").unwrap();

    // PRE-CALC
    let mut layer_qtys = Vec::with_capacity(params.grid_layers);
    let mut total_qty = 0.0;
    println!("🧮 Pre-calc... Target: {:02}:{:02}:00 UTC", params.target_hour, params.target_minute);
    
    for i in 0..params.grid_layers {
        let raw_qty = params.base_qty + (i as f64 * params.qty_increment);
        let qty = round_step(raw_qty, params.step_size);
        layer_qtys.push(qty);
        total_qty += qty;
    }

    let mut ctx = Context {
        state: State::Idle,
        open_orders: Vec::with_capacity(params.grid_layers),
        layer_qtys,
        total_exit_qty: total_qty,
    };

    loop {
        if config.should_stop() { break; }

        let now = SystemTime::now();
        let dt: DateTime<Utc> = now.into();
        let h = dt.hour();
        let m = dt.minute();
        let s = dt.second();
        let ms = dt.timestamp_subsec_millis();

        // PANIC CHECK
        if ctx.state == State::GridPlaced 
           && h == params.target_hour 
           && m == params.target_minute 
           && s >= params.panic_seconds 
        {
            println!("🚨 [PANIC] Timeout! Force exiting...");
            exit_all(&mut ctx, &c_api, &c_sec, &c_sym, &c_sell, cancel, place, params.step_size);
            ctx.state = State::Done;
        }

        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(event) => {
                match event.event_type {
                    0 => { // MARKET DATA
                        if ctx.state == State::Idle {
                            let (trig_h, trig_m) = if params.target_minute == 0 {
                                 let ph = if params.target_hour == 0 { 23 } else { params.target_hour - 1 };
                                 (ph, 59)
                            } else {
                                 (params.target_hour, params.target_minute - 1)
                            };

                            if h == trig_h && m == trig_m && s == 59 && ms >= (1000 - params.trigger_ms_before) {
                                if let Some(bt) = event.as_book_ticker() {
                                    let anchor = bt.bid_price;
                                    println!("⚡ TRIGGER! Anchor: {}", anchor);
                                    
                                    for i in 0..params.grid_layers {
                                        let pct_drop = params.initial_offset_pct + (i as f64 * params.step_pct);
                                        let raw_price = anchor * (1.0 - (pct_drop / 100.0));
                                        let price = round_step(raw_price, params.tick_size);
                                        let qty = ctx.layer_qtys[i];
                                        
                                        println!("   -> Limit BUY P:{} Q:{}", price, qty);
                                        place(
                                            c_api.as_ptr(), c_sec.as_ptr(), c_sym.as_ptr(),
                                            price, qty, c_buy.as_ptr(), 
                                            0, false, on_res
                                        );
                                    }
                                    ctx.state = State::GridPlaced;
                                }
                            }
                        }
                    },
                    2 => { // ORDER UPDATE
                        if let Some(o) = event.as_order() {
                            let st = o.status_char as char;
                            println!("🔔 ORDER UPDATE: ID={} Status={} Filled={} Price={}", 
                                     o.order_id, st, o.accumulated_qty, o.avg_price);
                                     
                            if o.side_char == b'B' && (st == 'N' || st == 'P') { // New or Partial
                                if !ctx.open_orders.contains(&o.order_id) {
                                    ctx.open_orders.push(o.order_id);
                                }
                            }
                        }
                    },
                    3 => { // FUNDING
                         if ctx.state == State::GridPlaced {
                             if let Some(_) = event.as_funding() {
                                 println!("💸 FUNDING EVENT! EXITING!");
                                 exit_all(&mut ctx, &c_api, &c_sec, &c_sym, &c_sell, cancel, place, params.step_size);
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
    cancel: CancelOrderFn, place: PlaceOrderFn, step_size: f64
) {
    println!("🛑 CANCELING {} ORDERS", ctx.open_orders.len());
    for id in &ctx.open_orders {
        cancel(api.as_ptr(), sec.as_ptr(), sym.as_ptr(), *id, on_res);
    }
    
    let raw_qty = ctx.total_exit_qty * 2.0;
    let safe_exit_qty = round_step(raw_qty, step_size); // <--- ОКРУГЛЯЕМ!
    
    println!("🏃 REDUCE ONLY SELL: {} (rounded)", safe_exit_qty);
    
    place(
        api.as_ptr(), sec.as_ptr(), sym.as_ptr(),
        0.0, safe_exit_qty, side.as_ptr(), 
        1, true, on_res
    );
}

unsafe extern "C" fn on_res(res: OrderResult) {
    if res.success {
        println!("✅ API OK: OrderID={}", res.order_id);
    } else {
        println!("❌ API FAIL: Code={}", res.error_code);
    }
}