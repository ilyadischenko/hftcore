mod types;
use types::*;

use crossbeam::channel::{Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime};
use std::ffi::CString;
use serde::Deserialize;

#[derive(Deserialize)]
struct Params {
    api_key: String,
    secret_key: String,
    buy_qty: f64,       // Сколько купить (напр 0.002 BTC)
    sell_qty: f64,      // Сколько попытаться продать (напр 0.004 BTC)
}

#[derive(PartialEq)]
enum State {
    Start,
    BuySent,
    WaitFill,
    SellSent,
    Done,
}

struct Context {
    state: State,
    buy_id: i64,
}

#[no_mangle]
pub unsafe extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    place: PlaceOrderFn,
    _cancel: CancelOrderFn,
    config: StrategyConfig
) -> i32 {
    let rx = &*rx_ptr;
    let params: Params = serde_json::from_str(config.params_str()).unwrap();
    let symbol = config.symbol();
    
    // C-Strings
    let c_api = CString::new(params.api_key.clone()).unwrap();
    let c_sec = CString::new(params.secret_key.clone()).unwrap();
    let c_sym = CString::new(symbol).unwrap();
    let c_buy = CString::new("BUY").unwrap();
    let c_sell = CString::new("SELL").unwrap();

    let mut ctx = Context { state: State::Start, buy_id: 0 };

    println!("🧪 [REDUCE TEST] Start. Buy: {}, Try Sell: {}", params.buy_qty, params.sell_qty);

    loop {
        if config.should_stop() { break; }

        // 1. SEND BUY (MARKET)
        if ctx.state == State::Start {
            println!("➡️ Sending BUY {}...", params.buy_qty);
            place(
                c_api.as_ptr(), c_sec.as_ptr(), c_sym.as_ptr(),
                0.0, params.buy_qty, c_buy.as_ptr(), 1, // 1=Market
                false, // reduce_only = FALSE
                on_buy
            );
            ctx.state = State::BuySent;
        }

        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(event) => {
                match event.event_type {
                    // ORDER UPDATE
                    2 => {
                        if let Some(o) = event.as_order() {
                            // Ждем пока купится
                            if ctx.state == State::BuySent && o.side_char == b'B' {
                                if o.status_char == b'N' {
                                     ctx.buy_id = o.order_id;
                                }
                                else if o.status_char == b'F' {
                                     println!("✅ Bought! ID={}", o.order_id);
                                     ctx.state = State::WaitFill;
                                }
                            }
                        }
                    },
                    _ => {}
                }
            },
            Err(_) => {}
        }
        
        // 2. SEND SELL (REDUCE ONLY)
        if ctx.state == State::WaitFill {
             // Ждем секунду для надежности (чтобы биржа обновила позицию)
             std::thread::sleep(Duration::from_millis(1000));
             
             println!("➡️ Sending REDUCE-ONLY SELL {}...", params.sell_qty);
             place(
                c_api.as_ptr(), c_sec.as_ptr(), c_sym.as_ptr(),
                0.0, params.sell_qty, c_sell.as_ptr(), 1, // 1=Market
                true, // reduce_only = TRUE !!!
                on_sell
            );
            ctx.state = State::Done;
        }
        
        if ctx.state == State::Done {
             // Даем время получить коллбэк и выходим
             std::thread::sleep(Duration::from_millis(2000));
             break;
        }
    }
    println!("🏁 Test finished");
    0
}

unsafe extern "C" fn on_buy(r: OrderResult) {
    if !r.success { println!("❌ Buy Failed: {}", r.error_code); }
}

unsafe extern "C" fn on_sell(r: OrderResult) {
    if !r.success { 
        println!("❌ Sell Failed: {}", r.error_code); 
    } else {
        println!("✅ Sell Sent! ID={}", r.order_id);
    }
}