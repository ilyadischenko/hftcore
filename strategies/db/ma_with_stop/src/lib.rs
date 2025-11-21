use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static RUNNING: AtomicBool = AtomicBool::new(true);

#[no_mangle]
pub extern "C" fn run() -> i32 {
    println!("🚀 Strategy started!");
    RUNNING.store(true, Ordering::Relaxed);
    
    let mut tick = 0;
    while RUNNING.load(Ordering::Relaxed) {
        tick += 1;
        println!("📊 Tick #{}", tick);
        std::thread::sleep(Duration::from_secs(1));
    }
    
    println!("✅ Strategy stopped");
    0
}

#[no_mangle]
pub extern "C" fn stop() {
    println!("🛑 Stop requested");
    RUNNING.store(false, Ordering::Relaxed);
}