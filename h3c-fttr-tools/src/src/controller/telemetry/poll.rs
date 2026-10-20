use super::Collector;
use crate::pon_engine::omci::platform::leases::{lock_onu, ChannelLease, StockOmccLease};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

pub(in crate::controller) fn start(id: u8, token: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name(format!("onu-{id}-poll"))
        .stack_size(256 * 1024)
        .spawn(move || {
        let mut collector = Collector::new(id, token.clone());
        let mut next = Instant::now();
        while !token.load(Ordering::Acquire) {
            if Instant::now() >= next {
                let result = (|| -> std::io::Result<()> {
                    let _owner = lock_onu(id)?;
                    let _stock = StockOmccLease::open(id)?;
                    let _channel = ChannelLease::open(id, 0)?;
                    collector.refresh();
                    Ok(())
                })();
                next = Instant::now() + Duration::from_secs(if result.is_ok() { 30 } else { 1 });
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
}
