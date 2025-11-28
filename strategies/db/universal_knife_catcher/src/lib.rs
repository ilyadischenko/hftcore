mod types;
use types::*;

// Universal Knife Catcher Pro — ловит ножи вниз и пробои вверх на ЛЮБОЙ монете
// Просто меняй SYMBOL и BASE_SIZE_CONTRACTS

use crossbeam::channel::Receiver;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicI64, Ordering};
use std::time::{Duration, Instant};
use std::ffi::CString;

static STOP_FLAG: AtomicBool = AtomicBool::new(false);
static mut PLACE_ORDER_FN: Option<PlaceOrderFn> = None;

// ====================== НАСТРОЙКИ ======================
const SYMBOL: &str = "SOLUSDT";           // ←←← МЕНЯЙ ТУТ МОНЕТУ (BONKUSDT, 1000PEPEUSDT, DOGEUSDT, BTCUSDT и т.д.)
const BASE_SIZE_CONTRACTS: f64 = 120.0;     // SOL ≈ 21k$, BONK ≈ 8000–15000, BTC ≈ 2.5

// Нож вниз (лонг)
const MIN_DROP_PCT: f64 = 0.74;
const MAX_DROP_PCT: f64 = 4.2;
const MAX_DROP_SEC: f64 = 3.1;

// Пробой вверх (шорт)
const MIN_PUMP_PCT: f64 = 0.81;
const MAX_PUMP_PCT: f64 = 4.5;
const MAX_PUMP_SEC: f64 = 2.6;

const TAKE_PROFIT_PCT: f64 = 0.72;
const STOP_LOSS_PCT: f64 = 0.48;
const DAILY_LOSS_LIMIT: f64 = -14000.0;

static HIGH_5S: AtomicU64 = AtomicU64::new(0);
static LOW_5S: AtomicU64 = AtomicU64::new(0);
static LAST_EXTREME_TIME: AtomicU64 = AtomicU64::new(0);

static IN_LONG: AtomicBool = AtomicBool::new(false);
static IN_SHORT: AtomicBool = AtomicBool::new(false);
static ENTRY_PRICE: AtomicU64 = AtomicU64::new(0);
static DAILY_PNL: AtomicI64 = AtomicI64::new(0);

static API_KEY: &str = "твой_api_key_фьючерсы";
static SECRET_KEY: &str = "твой_secret_фьючерсы";

struct KnifeCatcher {
    api_key: CString,
    secret_key: CString,
    symbol: CString,
    buy: CString,
    sell: CString,
    start: Instant,
}

impl KnifeCatcher {
    fn new() -> Self {
        Self {
            api_key: CString::new(API_KEY).unwrap(),
            secret_key: CString::new(SECRET_KEY).unwrap(),
            symbol: CString::new(SYMBOL).unwrap(),
            buy: CString::new("BUY").unwrap(),
            sell: CString::new("SELL").unwrap(),
            start: Instant::now(),
        }
    }

    fn p(u: u64) -> f64 { u as f64 / 1e8 }
    fn up(p: f64) -> u64 { (p * 1e8) as u64 }

    fn enter(&self, price: f64, is_long: bool) {
        if IN_LONG.load(Ordering::Relaxed) || IN_SHORT.load(Ordering::Relaxed) { return; }

        unsafe {
            PLACE_ORDER_FN.unwrap()(self.api_key.as_ptr(), self.secret_key.as_ptr(), self.symbol.as_ptr(), price, BASE_SIZE_CONTRACTS, if is_long { self.buy.as_ptr() } else { self.sell.as_ptr() }, callback);
        }

        ENTRY_PRICE.store(Self::up(price), Ordering::Relaxed);
        if is_long { IN_LONG.store(true, Ordering::Relaxed); }
        else { IN_SHORT.store(true, Ordering::Relaxed); }

        println!("{} {} @ ${price} — {} caught on {}", if is_long { "KNIFE LONG" } else { "PUMP SHORT" }, BASE_SIZE_CONTRACTS, price, if is_long { "нож" } else { "пробой" }, SYMBOL);
    }

    fn exit(&self, price: f64) {
        let entry = Self::p(ENTRY_PRICE.load(Ordering::Relaxed));
        let pnl_pct = if IN_LONG.load(Ordering::Relaxed) { (price - entry) / entry * 100.0 } else { (entry - price) / entry * 100.0 };

        let side = if IN_LONG.load(Ordering::Relaxed) { &self.sell } else { &self.buy };
        unsafe { PLACE_ORDER_FN.unwrap()(self.api_key.as_ptr(), self.secret_key.as_ptr(), self.symbol.as_ptr(), price, BASE_SIZE_CONTRACTS, side.as_ptr(), callback); }

        let pnl_usd = (price - entry).abs() * BASE_SIZE_CONTRACTS;
        DAILY_PNL.fetch_add(if pnl_pct > 0.0 { pnl_usd as i64 } else { -(pnl_usd as i64) }, Ordering::Relaxed);

        println!("EXIT @ ${price:.5} → {pnl_pct:+.2}% (${pnl_usd:.0}) | Daily P&L: ${}", DAILY_PNL.load(Ordering::Relaxed));

        IN_LONG.store(false, Ordering::Relaxed);
        IN_SHORT.store(false, Ordering::Relaxed);
    }
}

unsafe extern "C" fn callback(_: OrderResult) {}

#[no_mangle]
pub extern "C" fn run(rx_ptr: *mut Receiver<CEvent>, place_order: PlaceOrderFn, _cancel_order: CancelOrderFn) -> i32 {
    unsafe { PLACE_ORDER_FN = Some(place_order); }

    println!("UNIVERSAL KNIFE CATCHER STARTED → {SYMBOL} | {BASE_SIZE_CONTRACTS} contracts");

    let rx = unsafe { &*rx_ptr };
    let mut catcher = KnifeCatcher::new();

    while !STOP_FLAG.load(Ordering::Relaxed) {
        if DAILY_PNL.load(Ordering::Relaxed) as f64 <= DAILY_LOSS_LIMIT {
            println!("DAILY LOSS LIMIT — STOPPING");
            break;
        }

        if let Ok(event) = rx.recv_timeout(Duration::from_millis(30)) {
            if event.event_type == 1 { // trade stream
                let trade = unsafe { &event.data.trade };
                let price = trade.price;
                let now = catcher.start.elapsed().as_secs_f64();

                let high = KnifeCatcher::p(HIGH_5S.load(Ordering::Relaxed));
                let low = KnifeCatcher::p(LOW_5S.load(Ordering::Relaxed));

                if price > high || high == 0.0 { HIGH_5S.store(KnifeCatcher::up(price), Ordering::Relaxed); LAST_EXTREME_TIME.store(now as u64, Ordering::Relaxed); }
                if price < low || low == 0.0 { LOW_5S.store(KnifeCatcher::up(price), Ordering::Relaxed); LAST_EXTREME_TIME.store(now as u64, Ordering::Relaxed); }

                let in_trade = IN_LONG.load(Ordering::Relaxed) || IN_SHORT.load(Ordering::Relaxed);

                if !in_trade {
                    let drop_pct = if high > 0.0 { (high - price) / high * 100.0 } else { 0.0 };
                    let drop_time = now - LAST_EXTREME_TIME.load(Ordering::Relaxed) as f64;
                    if drop_pct >= MIN_DROP_PCT && drop_pct <= MAX_DROP_PCT && drop_time <= MAX_DROP_SEC && drop_time >= 0.3 {
                        catcher.enter(price, true);
                    }

                    let pump_pct = if low > 0.0 { (price - low) / low * 100.0 } else { 0.0 };
                    let pump_time = now - LAST_EXTREME_TIME.load(Ordering::Relaxed) as f64;
                    if pump_pct >= MIN_PUMP_PCT && pump_pct <= MAX_PUMP_PCT && pump_time <= MAX_PUMP_SEC && pump_time >= 0.3 {
                        catcher.enter(price, false);
                    }
                } else {
                    let entry = KnifeCatcher::p(ENTRY_PRICE.load(Ordering::Relaxed));
                    let profit = if IN_LONG.load(Ordering::Relaxed) { (price - entry) / entry * 100.0 } else { (entry - price) / entry * 100.0 };
                    if profit >= TAKE_PROFIT_PCT || profit <= -STOP_LOSS_PCT {
                        catcher.exit(price);
                    }
                }
            }
        }
    }

    println!("STRATEGY STOPPED | Daily P&L: ${}", DAILY_PNL.load(Ordering::Relaxed));
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    STOP_FLAG.store(true, Ordering::Relaxed);
}