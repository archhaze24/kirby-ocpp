use std::net::SocketAddr;

/// A loopback address that refuses connections.
///
/// On Linux and Windows the port stays bound without `listen` until the test process ends, so
/// connections are refused and a parallel test cannot take the port over. macOS lets
/// connections to such a port time out instead of refusing them, so there the port is
/// released at once; macOS picks ephemeral ports at random, which makes a reuse unlikely.
///
/// # Panics
///
/// Panics when no loopback socket can be bound.
#[must_use]
pub fn refused_addr() -> SocketAddr {
    if cfg!(target_os = "macos") {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        return listener.local_addr().expect("a bound address");
    }
    let socket = tokio::net::TcpSocket::new_v4().expect("a socket");
    socket
        .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("a loopback port");
    let addr = socket.local_addr().expect("a bound address");
    std::mem::forget(socket);
    addr
}

/// A `ws://` station URL on [`refused_addr`].
#[must_use]
pub fn refused_url() -> String {
    format!("ws://{}/ocpp", refused_addr())
}
