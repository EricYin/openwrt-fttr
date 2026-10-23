//! Bounded, ordered application transactions. Disk writes and SOAP parsing never
//! occupy an executor worker, and a cancelled HTTP response cannot cancel a
//! transaction halfway through persisting its sent marker.
use std::sync::OnceLock;
use tokio::sync::{mpsc, oneshot};

type Work = Box<dyn FnOnce() + Send>;

fn sender() -> &'static mpsc::Sender<Work> {
    static SENDER: OnceLock<mpsc::Sender<Work>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (sender, mut receiver) = mpsc::channel::<Work>(128);
        crate::runtime::spawn_service("ACS transactions", async move {
            loop {
                let work = tokio::select! {
                    work = receiver.recv() => work,
                    _ = crate::runtime::cancelled() => { receiver.close(); receiver.recv().await }
                };
                let Some(work) = work else { break };
                if let Err(error) = crate::runtime::spawn_blocking(work).await {
                    eprintln!("[ACS] transaction worker failed: {error}");
                }
            }
        });
        sender
    })
}

pub(super) async fn call<F, T>(work: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (reply, result) = oneshot::channel();
    sender()
        .try_send(Box::new(move || {
            // Admission is cancellable; once execution starts it is indivisible.
            if !reply.is_closed() {
                let _ = reply.send(work());
            }
        }))
        .map_err(|_| ())?;
    result.await.map_err(|_| ())
}
