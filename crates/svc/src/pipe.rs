use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use interprocess::local_socket::tokio::{RecvHalf, Stream, prelude::*};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions};
use interprocess::os::windows::local_socket::ListenerOptionsExt;
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use rkn_core::ipc::{self, Message, PIPE_NAME, Request};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Semaphore, broadcast, mpsc};

use crate::hub::Hub;

const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";
const MAX_LINE: u64 = 256 * 1024;
const MAX_CLIENTS: usize = 32;
const WRITE_LIMIT: Duration = Duration::from_secs(10);

pub async fn serve(hub: Arc<Hub>) -> anyhow::Result<()> {
    let sddl = widestring::U16CString::from_str(PIPE_SDDL)?;
    let listener = ListenerOptions::new()
        .name(PIPE_NAME.to_ns_name::<GenericNamespaced>()?)
        .security_descriptor(SecurityDescriptor::deserialize(&sddl)?)
        .create_tokio()
        .context("pipe")?;
    tracing::info!("pipe {PIPE_NAME} ready");
    let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
    loop {
        match listener.accept().await {
            Ok(conn) => {
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    tracing::debug!("pipe: больше {MAX_CLIENTS} клиентов, отказ");
                    continue;
                };
                let hub = hub.clone();
                tokio::spawn(async move {
                    let _slot = slot;
                    if let Err(e) = handle(hub, conn).await {
                        tracing::debug!("pipe client: {e:#}");
                    }
                });
            }
            Err(e) => tracing::warn!("pipe accept: {e}"),
        }
    }
}

async fn read_requests(recv: RecvHalf, tx: mpsc::Sender<Result<Request, String>>) -> anyhow::Result<()> {
    let mut reader = BufReader::new(recv);
    let mut line = String::new();
    loop {
        line.clear();
        let n = (&mut reader).take(MAX_LINE).read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') && n as u64 >= MAX_LINE {
            anyhow::bail!("request too long");
        }
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<Request>(&line).map_err(|e| format!("Неверный запрос: {e}"));
        if tx.send(parsed).await.is_err() {
            return Ok(());
        }
    }
}

async fn send_all<W: AsyncWrite + Unpin>(send: &mut W, bytes: &[u8]) -> anyhow::Result<()> {
    tokio::time::timeout(WRITE_LIMIT, send.write_all(bytes)).await.context("клиент не читает ответ")??;
    Ok(())
}

async fn handle(hub: Arc<Hub>, conn: Stream) -> anyhow::Result<()> {
    let (recv, mut send) = conn.split();
    let (tx, mut requests) = mpsc::channel(8);
    let reader = tokio::spawn(read_requests(recv, tx));
    let mut events: Option<broadcast::Receiver<Message>> = None;

    let result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { return Ok(()) };
                    match request {
                        Err(message) => send_all(&mut send, ipc::encode(&Message::Error { message }).as_bytes()).await?,
                        Ok(Request::Subscribe) => {
                            let (status, rx) = hub.subscribe();
                            events = Some(rx);
                            send_all(&mut send, ipc::encode(&Message::Status(Box::new(status))).as_bytes()).await?;
                        }
                        Ok(request) => {
                            let hub = hub.clone();
                            let reply = tokio::task::spawn_blocking(move || hub.handle(request)).await?;
                            send_all(&mut send, ipc::encode(&reply).as_bytes()).await?;
                        }
                    }
                }
                message = recv_event(&mut events), if events.is_some() => {
                    match message {
                        // Events only go to the log file; the window follows the status.
                        Ok(Message::Event(_)) => {}
                        Ok(message) => send_all(&mut send, ipc::encode(&message).as_bytes()).await?,
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let hub = hub.clone();
                            let reply = tokio::task::spawn_blocking(move || hub.handle(Request::Status)).await?;
                            send_all(&mut send, ipc::encode(&reply).as_bytes()).await?;
                        }
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
            }
        }
    }
    .await;
    reader.abort();
    result
}

async fn recv_event(events: &mut Option<broadcast::Receiver<Message>>) -> Result<Message, broadcast::error::RecvError> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}
