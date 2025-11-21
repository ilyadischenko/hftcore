use strategy_api::*;
use std::sync::Arc;

pub struct MinimalStrategy;

impl Strategy for MinimalStrategy {
    fn on_tick(&mut self, _e: MarketEvent, _t: Arc<dyn TradingApi>) {}
    fn name(&self) -> &str { "Minimal" }
    fn symbol(&self) -> &str { "BTCUSDT" }
}

#[no_mangle]
pub extern "C" fn create_strategy() -> *mut dyn Strategy {
    Box::into_raw(Box::new(MinimalStrategy))
}