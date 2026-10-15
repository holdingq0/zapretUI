use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::hub::Hub;
use crate::{pipe, tuner, update};

pub async fn run(shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
    let (jobs_tx, jobs) = tokio::sync::mpsc::unbounded_channel();
    let (verify_tx, verify) = tokio::sync::mpsc::unbounded_channel();
    let (update_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let hub = tokio::task::spawn_blocking(move || Arc::new(Hub::new(jobs_tx, verify_tx, update_tx))).await?;
    {
        let hub = hub.clone();
        tokio::task::spawn_blocking(move || hub.apply()).await?;
    }
    let mut server = tokio::spawn(pipe::serve(hub.clone()));
    let tuner = tokio::spawn(tuner::run(hub.clone(), jobs));
    let verifier = tokio::spawn(tuner::verifier(hub.clone(), verify));
    let updater = tokio::spawn(update::run(hub.clone(), updates));
    let ticker = {
        let hub = hub.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(3));
            loop {
                every.tick().await;
                let hub = hub.clone();
                let _ = tokio::task::spawn_blocking(move || hub.tick()).await;
            }
        })
    };

    let mut quit = hub.quits();
    let result = tokio::select! {
        _ = shutdown => Ok(()),
        _ = quit.wait_for(|q| *q) => Ok(()),
        r = &mut server => match r {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e.into()),
        },
    };
    server.abort();
    ticker.abort();
    tuner.abort();
    verifier.abort();
    updater.abort();
    tokio::task::spawn_blocking(move || hub.shutdown()).await?;
    tracing::info!("stopped");
    result
}
