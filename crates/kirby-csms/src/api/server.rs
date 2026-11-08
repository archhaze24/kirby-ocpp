//! The connection layer: the accept loop, the per-connection socket wrapper with its stall,
//! deadline and drain rules, and the shutdown.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::Extension;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::Sleep;

use super::limits::{DRAIN_BYTES, DRAIN_TIME, HEAD_TIMEOUT, Limits, SHUTDOWN_GRACE, Streaming};

/// An accepted connection's socket. It gives up when
/// - a write makes no progress for `after` (a client that stopped reading),
/// - the response takes longer than `total` from its first write, unless the handler marked
///   the connection as [`Streaming`],
///
/// and at the end it drains the client's unread request data for a moment (see
/// [`DRAIN_TIME`]) before the socket is closed.
pub(super) struct StallIo<T> {
    inner: T,
    /// How long a write may make no progress.
    after: Duration,
    /// Armed while a write is pending; cleared by any write progress.
    stall: Option<Pin<Box<Sleep>>>,
    /// How long a response may take to be written.
    total: Duration,
    /// Armed by the first write.
    deadline: Option<Pin<Box<Sleep>>>,
    streaming: Arc<AtomicBool>,
    /// The write side was shut down; the drain is running.
    shut: bool,
    drain: Option<Pin<Box<Sleep>>>,
    pub(super) drained: usize,
}

impl<T> StallIo<T> {
    pub(super) fn new(inner: T, limits: Limits, streaming: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            after: limits.write_stall,
            stall: None,
            total: limits.response_deadline,
            deadline: None,
            streaming,
            shut: false,
            drain: None,
            drained: 0,
        }
    }

    /// Applies the stall and deadline rules to the result of a write-side poll.
    fn guard<R>(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<R>>,
    ) -> Poll<io::Result<R>> {
        if !self.streaming.load(Ordering::Acquire) {
            let total = self.total;
            let deadline = self
                .deadline
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(total)));
            if deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
            }
        }
        match polled {
            Poll::Pending => {
                let sleep = self
                    .stall
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(self.after)));
                if sleep.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Err(io::ErrorKind::TimedOut.into()))
                } else {
                    Poll::Pending
                }
            }
            ready @ Poll::Ready(_) => {
                self.stall = None;
                ready
            }
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for StallIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncWrite for StallIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.guard(cx, polled)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_flush(cx);
        this.guard(cx, polled)
    }

    /// Shuts the write side down, then reads and discards what the client still sends for up
    /// to [`DRAIN_TIME`] and [`DRAIN_BYTES`] (or until it closes).
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.shut {
            let polled = Pin::new(&mut this.inner).poll_shutdown(cx);
            match this.guard(cx, polled) {
                Poll::Ready(Ok(())) => this.shut = true,
                other => return other,
            }
        }
        let mut scratch = [0_u8; 4096];
        while this.drained < DRAIN_BYTES {
            let mut buf = ReadBuf::new(&mut scratch);
            match Pin::new(&mut this.inner).poll_read(cx, &mut buf) {
                Poll::Ready(Ok(())) if !buf.filled().is_empty() => {
                    this.drained += buf.filled().len();
                }
                Poll::Ready(_) => return Poll::Ready(Ok(())),
                Poll::Pending => break,
            }
        }
        if this.drained >= DRAIN_BYTES {
            return Poll::Ready(Ok(()));
        }
        let drain = this
            .drain
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(DRAIN_TIME)));
        if drain.as_mut().poll(cx).is_ready() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

/// Turns on TCP keepalive (first probe after 30 s idle, then every 10 s, 3 probes) so that a
/// vanished peer (half-open connection) is detected and its slot freed. Best effort.
fn enable_keepalive(stream: &TcpStream) {
    let params = TcpKeepalive::new().with_time(Duration::from_secs(30));
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    let params = params.with_interval(Duration::from_secs(10));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let params = params.with_retries(3);
    let _ = SockRef::from(stream).set_tcp_keepalive(&params);
}

/// Accepts connections until `stopped`, one task each (tracked in a [`JoinSet`]). A connection
/// holds a slot of the cap, gets [`HEAD_TIMEOUT`] from hyper for the request head and carries
/// one request. After the stop signal the listener is dropped at once, open requests get
/// [`SHUTDOWN_GRACE`], then every connection is aborted and awaited: nothing outlives this
/// future.
pub(super) async fn serve(
    listener: TcpListener,
    app: Router,
    slots: Arc<Semaphore>,
    limits: Limits,
    mut stopped: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = wait_stopped(&mut stopped) => break,
            Some(_) = connections.join_next() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    // Out of descriptors or similar: do not spin (but do not miss a stop).
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(50)) => continue,
                        () = wait_stopped(&mut stopped) => break,
                    }
                };
                // Over the cap the socket is dropped (closed) at once.
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    continue;
                };
                enable_keepalive(&stream);
                let streaming = Arc::new(AtomicBool::new(false));
                let io = TokioIo::new(StallIo::new(stream, limits, Arc::clone(&streaming)));
                let service = TowerToHyperService::new(
                    app.clone().layer(Extension(Streaming(streaming))),
                );
                let mut stop = stopped.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let connection = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEAD_TIMEOUT)
                        .keep_alive(false)
                        .serve_connection(io, service);
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = connection.as_mut() => {}
                        () = wait_stopped(&mut stop) => {
                            connection.as_mut().graceful_shutdown();
                            let _ = connection.await;
                        }
                    }
                });
            }
        }
    }
    drop(listener);
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    connections.shutdown().await;
}

/// Resolves once the server stops.
pub(super) async fn wait_stopped(stopped: &mut watch::Receiver<bool>) {
    let _ = stopped.wait_for(|stop| *stop).await;
}
