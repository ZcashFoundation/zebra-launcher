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

/// The largest header list a call may send: hyper's default, which zebrad's endpoint uses. A
/// larger one ends the call or the connection before Zeal holds it.
const MAX_HEADER_LIST: u32 = 16 * 1024;

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
        .max_header_list_size(MAX_HEADER_LIST)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::{mpsc, watch};

    use super::*;
    use crate::{Timers, bandwidth::Bucket, limits::Counters};

    fn context() -> Arc<Context> {
        Arc::new(Context {
            name: "zec.test".into(),
            upstream: "127.0.0.1:9".parse().unwrap(),
            timers: Timers::default(),
            not_outside: Vec::new(),
            cert: watch::channel(None).1,
            challenge: watch::channel(None).1,
            mode: watch::channel(Mode::Unavailable).1,
            counters: Arc::new(Counters::default()),
            bucket: Bucket::new(None),
            events: mpsc::channel(1).0,
        })
    }

    /// Sends one call with a header of `size` bytes and returns its `grpc-status`, or `None`
    /// when the call was refused or the connection ended.
    async fn call(size: usize) -> Option<String> {
        let (client, server) = tokio::io::duplex(1 << 20);
        let (_mode_tx, mut mode) = watch::channel(Mode::Unavailable);
        let ctx = context();
        tokio::spawn(async move { run(server, ctx, &mut mode).await });
        let (mut send, connection) = h2::client::handshake(client).await.ok()?;
        tokio::spawn(connection);
        let request = http::Request::builder()
            .method("POST")
            .uri("https://zec.test/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLightdInfo")
            .header("content-type", "application/grpc")
            .header("x-padding", "a".repeat(size))
            .body(())
            .unwrap();
        let (response, _body) = send.send_request(request, true).ok()?;
        let response = tokio::time::timeout(Duration::from_secs(10), response)
            .await
            .expect("an answer in time")
            .ok()?;
        response
            .headers()
            .get("grpc-status")
            .and_then(|status| status.to_str().ok())
            .map(str::to_string)
    }

    #[tokio::test]
    async fn calls_are_answered_unless_their_headers_are_too_large() {
        assert_eq!(call(100).await.as_deref(), Some(UNAVAILABLE));
        assert_eq!(
            call(32 * 1024).await,
            None,
            "a 32 KiB header list is refused"
        );
    }
}
