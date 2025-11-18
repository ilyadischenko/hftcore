use strategy_api::*;
use std::sync::Arc;

pub struct MAStrategy {
    prices: Vec<f64>,
}

impl Strategy for MAStrategy {
    fn on_tick(&mut self, event: MarketEvent, trading: Arc<dyn TradingApi>) {
        println!("Price: {}", event.bid_price);
    }
    fn name(&self) -> &str { "MA" }
    fn symbol(&self) -> &str { "BTCUSDT" }
}

#[no_mangle]
pub extern "C" fn create_strategy() -> *mut dyn Strategy {
    Box::into_raw(Box::new(MAStrategy { prices: vec![] }))
}