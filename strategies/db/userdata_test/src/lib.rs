mod types;
use types::*;

use crossbeam::channel::Receiver;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct Params {
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    secret_key: String,
}

struct Stats {
    total: u64,
    orders: u64,
    funding: u64,
    latency_sum: u64,
    latency_count: u64,
    latency_min: u64,
    latency_max: u64,
}

impl Stats {
    fn new() -> Self {
        Self { total: 0, orders: 0, funding: 0, latency_sum: 0, latency_count: 0, latency_min: u64::MAX, latency_max: 0 }
    }
    fn record(&mut self, event_time: u64) {
        let now = now_ms();
        if event_time > 0 && now >= event_time {
            let lat = now - event_time;
            self.latency_sum += lat;
            self.latency_count += 1;
            self.latency_min = self.latency_min.min(lat);
            self.latency_max = self.latency_max.max(lat);
        }
    }
    fn avg(&self) -> f64 {
        if self.latency_count > 0 { self.latency_sum as f64 / self.latency_count as f64 } else { 0.0 }
    }
    fn print(&self) {
        println!("\n═══ STATS ═══");
        println!("Total: {} | Orders: {} | Funding: {}", self.total, self.orders, self.funding);
        println!("Latency: avg={:.1}ms min={}ms max={}ms", self.avg(), if self.latency_min == u64::MAX { 0 } else { self.latency_min }, self.latency_max);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

#[no_mangle]
pub extern "C" fn run(
    rx_ptr: *mut Receiver<CEvent>,
    _place_order: PlaceOrderFn,
    _cancel_order: CancelOrderFn,
    config: StrategyConfig,
) -> i32 {
    let rx = unsafe { &*rx_ptr };
    let symbol = config.symbol().to_string();

    println!("═══ USER DATA TEST | {} ═══", symbol);
    println!("User Data: {}", if config.has_user_data() { "CONNECTED ✅" } else { "NOT CONNECTED ❌" });
    println!("Waiting for events...\n");

    let mut stats = Stats::new();
    let mut last_print = std::time::Instant::now();

    loop {
        if config.should_stop() { break; }

        // Drain market data
        let _ = rx.recv_timeout(Duration::from_millis(10));

        // User Data
        while let Some(event) = config.try_recv_user_data() {
            stats.total += 1;
            let now = now_ms();

            match event {
                UserDataEvent::OrderUpdate(ou) => {
                    stats.orders += 1;
                    stats.record(ou.event_time);
                    let lat = now.saturating_sub(ou.event_time);
                    println!("📦 ORDER | {}ms | {} {} {} @ {} | status={}",
                        lat, ou.order.side, ou.order.quantity, ou.order.symbol,
                        ou.order.average_price, ou.order.status);
                }
                UserDataEvent::AccountUpdate(au) => {
                    stats.record(au.event_time);
                    let lat = now.saturating_sub(au.event_time);
                    if au.reason == "FundingFee" {
                        stats.funding += 1;
                        for b in &au.balances {
                            println!("💰 FUNDING | {}ms | {} change={}", lat, b.asset, b.balance_change);
                        }
                    } else {
                        println!("📊 ACCOUNT | {}ms | reason={}", lat, au.reason);
                    }
                }
                UserDataEvent::MarginCall(mc) => {
                    stats.record(mc.event_time);
                    println!("⚠️ MARGIN CALL | wallet={}", mc.cross_wallet_balance);
                }
                UserDataEvent::Unknown { raw } => {
                    println!("❓ UNKNOWN | {}", &raw[..raw.len().min(100)]);
                }
            }
        }

        if last_print.elapsed() > Duration::from_secs(30) {
            stats.print();
            last_print = std::time::Instant::now();
        }
    }

    stats.print();
    println!("🛑 Stopped");
    0
}