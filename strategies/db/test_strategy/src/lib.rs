use crossbeam::channel::Receiver;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::VecDeque;
use std::time::SystemTime;

static STOP_FLAG: AtomicBool = AtomicBool::new(false);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CEvent {
    pub event_type: u8,
    pub data: CEventData,
    pub received_at_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union CEventData {
    pub book_ticker: CBookTicker,
    pub trade: CTrade,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CBookTicker {
    pub symbol: [u8; 16],
    pub symbol_len: u8,
    pub bid_price: f64,
    pub ask_price: f64,
    pub bid_qty: f64,
    pub ask_qty: f64,
    pub time: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CTrade {
    pub symbol: [u8; 16],
    pub symbol_len: u8,
    pub price: f64,
    pub qty: f64,
    pub time: i64,
}

impl CBookTicker {
    fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
    fn mid_price(&self) -> f64 { (self.bid_price + self.ask_price) / 2.0 }
    fn spread(&self) -> f64 { self.ask_price - self.bid_price }
}

impl CTrade {
    fn symbol_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.symbol[..self.symbol_len as usize]) }
    }
}

struct Strategy {
    ma_period: usize,
    threshold: f64,
    prices: VecDeque<f64>,
    position: f64,
    entry_price: f64,
    events_received: u64,
    trades_seen: u64,
    signals_generated: u64,
    latencies: Vec<f64>,
    max_latency: f64,
    min_latency: f64,
}

impl Strategy {
    fn new() -> Self {
        Self {
            ma_period: 20,
            threshold: 0.1,
            prices: VecDeque::with_capacity(100),
            position: 0.0,
            entry_price: 0.0,
            events_received: 0,
            trades_seen: 0,
            signals_generated: 0,
            latencies: Vec::with_capacity(100000),
            max_latency: 0.0,
            min_latency: f64::MAX,
        }
    }
    fn on_book_ticker(&mut self, bt: &CBookTicker, received_at_ns: u64) {
        let now_ns = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos() as u64;
        let latency_ns = now_ns - received_at_ns;
        let latency_us = latency_ns as f64 / 1000.0;
        self.latencies.push(latency_us);
        if latency_us > self.max_latency { self.max_latency = latency_us; }
        if latency_us < self.min_latency { self.min_latency = latency_us; }
        self.events_received += 1;
        let mid = bt.mid_price();
        self.prices.push_back(mid);
        if self.prices.len() > 100 { self.prices.pop_front(); }
        if self.prices.len() >= self.ma_period { self.check_signals(bt); }
    }
    fn on_trade(&mut self, _trade: &CTrade, _received_at_ns: u64) {
        self.trades_seen += 1;
    }
    fn check_signals(&mut self, bt: &CBookTicker) {
        let ma = self.calculate_ma();
        let price = bt.mid_price();
        let dev = ((price - ma) / ma) * 100.0;
        if dev > self.threshold && self.position <= 0.0 {
            self.signals_generated += 1;
            self.position = 1.0;
            self.entry_price = bt.ask_price;
        } else if dev < -self.threshold && self.position >= 0.0 {
            self.signals_generated += 1;
            self.position = -1.0;
            self.entry_price = bt.bid_price;
        }
    }
    fn calculate_ma(&self) -> f64 {
        let sum: f64 = self.prices.iter().rev().take(self.ma_period).sum();
        sum / self.ma_period as f64
    }
    fn calculate_avg_latency(&self) -> f64 {
        if self.latencies.is_empty() { return 0.0; }
        self.latencies.iter().sum::<f64>() / self.latencies.len() as f64
    }
    fn print_stats(&self) {
        println!("\n═══════════════════════════════════════════");
        println!("📊 STRATEGY STATISTICS");
        println!("═══════════════════════════════════════════");
        println!("Events processed:     {}", self.events_received);
        println!("Trades seen:          {}", self.trades_seen);
        println!("Signals generated:    {}", self.signals_generated);
        println!("Current position:     {}", self.position);
        if !self.latencies.is_empty() {
            let mut sorted = self.latencies.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let len = sorted.len();
            let p50 = sorted[len / 2];
            let p95 = sorted[(len as f64 * 0.95) as usize];
            let p99 = sorted[(len as f64 * 0.99) as usize];
            let p999 = sorted[(len as f64 * 0.999) as usize];
            println!("\n⏱️  LATENCY METRICS (microseconds)");
            println!("───────────────────────────────────────────");
            println!("Average:              {:.1} µs", self.calculate_avg_latency());
            println!("Min:                  {:.1} µs", self.min_latency);
            println!("Max:                  {:.1} µs", self.max_latency);
            println!("P50 (median):         {:.1} µs", p50);
            println!("P95:                  {:.1} µs", p95);
            println!("P99:                  {:.1} µs", p99);
            println!("P99.9:                {:.1} µs", p999);
        }
        println!("═══════════════════════════════════════════\n");
    }
}

#[no_mangle]
pub extern "C" fn run(rx_ptr: *mut Receiver<CEvent>) -> i32 {
    println!("🚀 Strategy started (silent mode)");
    let rx = unsafe { &*rx_ptr };
    let mut strategy = Strategy::new();
    while !STOP_FLAG.load(Ordering::Relaxed) {
        match rx.recv() {
            Ok(event) => {
                match event.event_type {
                    0 => { let bt = unsafe { &event.data.book_ticker }; strategy.on_book_ticker(bt, event.received_at_ns); }
                    1 => { let t = unsafe { &event.data.trade }; strategy.on_trade(t, event.received_at_ns); }
                    _ => {}
                }
            }
            Err(_) => break,
        }
    }
    strategy.print_stats();
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    STOP_FLAG.store(true, Ordering::Relaxed);
}