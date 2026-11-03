//! Front door v1: the decrypted bytes go to the backend as they are. HTTP/2 over TLS and
//! HTTP/2 with prior knowledge carry the same frames, so nothing here parses what it forwards.
//!
//! One task owns both streams and moves whichever side has bytes, so the TLS stream is never
//! split. The upload to the client is metered; a read in either direction counts as activity.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::watch,
    time::Instant,
};

use crate::{Context, Mode, limits::Counters};

/// How long the front door waits to reach the backend.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// One read's worth.
const BUFFER: usize = 16 * 1024;

/// Waits until neither direction moved a byte for `idle`.
async fn idle(activity: &AtomicU64, start: Instant, idle: Duration) {
    loop {
        let last = Duration::from_millis(activity.load(Ordering::Relaxed));
        let quiet = start.elapsed().saturating_sub(last);
        if quiet >= idle {
            return;
        }
        tokio::time::sleep(idle - quiet).await;
    }
}

/// Moves bytes both ways until either side closes or fails.
async fn pump<S>(
    client: &mut S,
    backend: &mut TcpStream,
    ctx: &Context,
    activity: &AtomicU64,
    start: Instant,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut from_client = vec![0u8; BUFFER];
    let mut from_backend = vec![0u8; BUFFER];
    let mut debt = 0usize;
    let touch = || activity.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
    loop {
        // `read` is cancel-safe: the branch that loses keeps its bytes for the next round.
        tokio::select! {
            read = client.read(&mut from_client) => {
                let n = read?;
                if n == 0 {
                    return backend.shutdown().await;
                }
                touch();
                backend.write_all(&from_client[..n]).await?;
                Counters::add(&ctx.counters.bytes_down, n as u64);
            }
            read = backend.read(&mut from_backend) => {
                let n = read?;
                if n == 0 {
                    return client.shutdown().await;
                }
                touch();
                ctx.bucket.spend(n, &mut debt).await;
                client.write_all(&from_backend[..n]).await?;
                Counters::add(&ctx.counters.bytes_up, n as u64);
            }
        }
    }
}

/// Forwards `client` to the backend until either side closes, the connection idles or ages
/// out, or the front door's mode changes.
pub(crate) async fn run<S>(mut client: S, ctx: Arc<Context>, mode: &mut watch::Receiver<Mode>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let connect = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(ctx.upstream));
    let mut backend = match connect.await {
        Ok(Ok(backend)) => backend,
        // The backend is restarting; the service actor sees that too.
        _ => return,
    };
    let _ = backend.set_nodelay(true);
    let start = Instant::now();
    let activity = AtomicU64::new(0);
    tokio::select! {
        _ = pump(&mut client, &mut backend, &ctx, &activity, start) => {}
        _ = idle(&activity, start, ctx.timers.idle) => {}
        _ = tokio::time::sleep(ctx.timers.max_age) => {}
        _ = mode.changed() => {}
    }
}
