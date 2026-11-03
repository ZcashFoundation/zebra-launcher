//! The `Unavailable` mode: the front door completes the handshake and answers every gRPC call
//! itself, with a trailers-only `UNAVAILABLE` (status 14), so wallets get a clear error and
//! try again later. Nothing reaches the backend.

use std::sync::Arc;

use bytes::Bytes;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
};

use crate::{Context, Mode};

/// What wallets are told while the backend is not ready.
const MESSAGE: &str = "the light client server is catching up; try again soon";

/// gRPC's `UNAVAILABLE`.
const UNAVAILABLE: &str = "14";

/// At most this many calls at once on one connection.
const MAX_STREAMS: u32 = 16;

fn response() -> http::Response<()> {
    let mut response = http::Response::new(());
    let headers = response.headers_mut();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/grpc"),
    );
    headers.insert("grpc-status", http::HeaderValue::from_static(UNAVAILABLE));
    headers.insert("grpc-message", http::HeaderValue::from_static(MESSAGE));
    response
}

/// Answers every call on `client` until it closes, idles or ages out, or the mode changes.
pub(crate) async fn run<S>(client: S, ctx: Arc<Context>, mode: &mut watch::Receiver<Mode>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let handshake = h2::server::Builder::new()
        .max_concurrent_streams(MAX_STREAMS)
        .handshake::<_, Bytes>(client);
    let Ok(Ok(mut connection)) = tokio::time::timeout(ctx.timers.handshake, handshake).await else {
        return;
    };
    let answer = async {
        while let Some(Ok((request, mut respond))) = connection.accept().await {
            drop(request);
            // A trailers-only response: the headers carry the status and end the stream.
            let _ = respond.send_response(response(), true);
        }
    };
    tokio::select! {
        _ = answer => {}
        // Calls here are answered at once, so a connection is never busy for long.
        _ = tokio::time::sleep(ctx.timers.idle) => {}
        _ = tokio::time::sleep(ctx.timers.max_age) => {}
        _ = mode.changed() => {}
    }
}
