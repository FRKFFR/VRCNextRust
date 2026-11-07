//! OSC send/receive for VRChat (M7).
//!
//! UDP only, loopback by default: VRChat listens for OSC on
//! `127.0.0.1:9000` and sends to the configured out-port (default 9001).
//! All packets are encoded/decoded with `rosc`; sockets are tokio
//! `UdpSocket`s so the listener can run as a background task.

pub mod chatbox;
pub mod params;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;

use futures_util::Stream;
pub use rosc::OscMessage;
use rosc::{decoder, encoder, OscPacket, OscType};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// VRChat's default OSC input port.
pub const DEFAULT_INPUT_PORT: u16 = 9000;
/// VRChat's default OSC output port (where it sends avatar/parameter data).
pub const DEFAULT_OUTPUT_PORT: u16 = 9001;
/// Loopback address used for all OSC traffic.
pub const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// `127.0.0.1:<port>` helper.
pub fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(LOOPBACK, port)
}

/// The default VRChat OSC target (`127.0.0.1:9000`).
pub fn default_target() -> SocketAddr {
    loopback(DEFAULT_INPUT_PORT)
}

/// OSC failures: codec errors from `rosc`, socket errors from tokio.
#[derive(Debug, thiserror::Error)]
pub enum OscError {
    /// Packet encode/decode failed.
    #[error("OSC codec error: {0}")]
    Codec(#[from] rosc::OscError),
    /// UDP socket operation failed.
    #[error("OSC socket error: {0}")]
    Io(#[from] std::io::Error),
}

/// A bound UDP socket that sends OSC messages to a fixed target.
///
/// `bind` picks an ephemeral local port on the wildcard address matching
/// the target's IP family, so the client works for loopback and LAN targets.
pub struct OscClient {
    socket: UdpSocket,
    target: SocketAddr,
}

impl std::fmt::Debug for OscClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OscClient")
            .field("target", &self.target)
            .field("local", &self.socket.local_addr())
            .finish()
    }
}

impl OscClient {
    /// Bind an ephemeral local socket and send to `target`.
    pub async fn bind(target: SocketAddr) -> Result<Self, OscError> {
        let bind_addr = if target.is_ipv4() {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        };
        let socket = UdpSocket::bind(bind_addr).await?;
        Ok(Self { socket, target })
    }

    /// Client targeting VRChat's default input port.
    pub async fn vrchat() -> Result<Self, OscError> {
        Self::bind(default_target()).await
    }

    /// The remote address messages are sent to.
    pub fn target(&self) -> SocketAddr {
        self.target
    }

    /// The local socket address.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Encode and send one OSC message.
    pub async fn send(&self, message: &OscMessage) -> Result<(), OscError> {
        let bytes = encoder::encode(&OscPacket::Message(message.clone()))?;
        self.socket.send_to(&bytes, self.target).await?;
        Ok(())
    }

    /// Convenience: send `addr` with `args`.
    pub async fn send_to(&self, addr: &str, args: Vec<OscType>) -> Result<(), OscError> {
        self.send(&OscMessage {
            addr: addr.to_string(),
            args,
        })
        .await
    }
}

/// A bound UDP socket receiving OSC messages as a stream.
///
/// Created by [`OscListener::bind`]/[`OscListener::bind_addr`]. The
/// receiver task exits when the returned stream is dropped (its channel
/// receiver closes, the socket is released and the port freed).
pub struct OscListener {
    local_addr: SocketAddr,
}

impl std::fmt::Debug for OscListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OscListener")
            .field("local", &self.local_addr)
            .finish()
    }
}

/// Stream of decoded OSC messages (bundles flattened to their messages).
pub type OscMessageStream = Pin<Box<dyn Stream<Item = OscMessage> + Send>>;

/// Capacity of the listener's internal queue; bursts beyond this are
/// dropped (OSC is lossy anyway).
const LISTENER_QUEUE: usize = 256;
/// UDP datagram buffer — OSC packets fit in one MTU.
const RECV_BUF: usize = 65535;

/// Flatten an [`OscPacket`] into its [`OscMessage`]s (bundle contents
/// included, recursively).
fn flatten_packet(packet: OscPacket, out: &mut Vec<OscMessage>) {
    match packet {
        OscPacket::Message(msg) => out.push(msg),
        OscPacket::Bundle(bundle) => {
            for inner in bundle.content {
                flatten_packet(inner, out);
            }
        }
    }
}

impl OscListener {
    /// Bind `127.0.0.1:port` and return the listener plus a message stream.
    /// Port `0` picks an ephemeral port — read it via [`Self::local_addr`].
    pub async fn bind(port: u16) -> Result<(Self, OscMessageStream), OscError> {
        Self::bind_addr(loopback(port)).await
    }

    /// Bind an explicit address (e.g. `127.0.0.1:0` for tests).
    pub async fn bind_addr(addr: SocketAddr) -> Result<(Self, OscMessageStream), OscError> {
        let socket = UdpSocket::bind(addr).await?;
        let local_addr = socket.local_addr()?;
        let (tx, rx) = mpsc::channel::<OscMessage>(LISTENER_QUEUE);
        tokio::spawn(async move {
            let mut buf = vec![0u8; RECV_BUF];
            loop {
                tokio::select! {
                    // The stream owns `rx`; dropping it (e.g. the caller
                    // stops listening) resolves `closed` and frees the
                    // socket/port instead of leaking the task.
                    _ = tx.closed() => break,
                    recv = socket.recv_from(&mut buf) => match recv {
                        Ok((n, _peer)) => match decoder::decode_udp(&buf[..n]) {
                            Ok((_rest, packet)) => {
                                let mut messages = Vec::new();
                                flatten_packet(packet, &mut messages);
                                for msg in messages {
                                    // Drop on a full queue rather than
                                    // stalling the socket reader.
                                    if tx.try_send(msg).is_err() {
                                        tracing::debug!(
                                            "OSC listener queue full, dropping message"
                                        );
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!("undecodable OSC datagram: {e}");
                            }
                        },
                        Err(e) => {
                            tracing::debug!("OSC listener recv error: {e}");
                            break;
                        }
                    },
                }
            }
        });
        let stream = Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|msg| (msg, rx))
        }));
        Ok((Self { local_addr }, stream))
    }

    /// The address the listener is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use rosc::OscType;

    #[tokio::test]
    async fn client_listener_roundtrip_loopback() {
        let (listener, mut stream) = OscListener::bind(0).await.unwrap();
        let port = listener.local_addr().port();
        let client = OscClient::bind(loopback(port)).await.unwrap();

        client
            .send_to("/avatar/parameters/Speed", vec![OscType::Float(0.5)])
            .await
            .unwrap();

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("timed out waiting for OSC message")
            .expect("stream ended");
        assert_eq!(msg.addr, "/avatar/parameters/Speed");
        assert_eq!(msg.args, vec![OscType::Float(0.5)]);
    }

    #[tokio::test]
    async fn listener_port_zero_is_ephemeral() {
        let (l1, _s1) = OscListener::bind(0).await.unwrap();
        let (l2, _s2) = OscListener::bind(0).await.unwrap();
        assert_ne!(l1.local_addr().port(), l2.local_addr().port());
    }

    #[tokio::test]
    async fn dropping_stream_frees_the_port() {
        let port = {
            let (listener, stream) = OscListener::bind(0).await.unwrap();
            let port = listener.local_addr().port();
            drop(stream);
            port
        };
        // The receiver task exits asynchronously once it notices the
        // closed channel; poll until the port can be rebound.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match OscListener::bind(port).await {
                Ok((_listener, _stream)) => break,
                Err(_) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(e) => panic!("port {port} still bound after stream drop: {e}"),
            }
        }
    }

    #[tokio::test]
    async fn garbage_datagrams_are_ignored() {
        let (listener, mut stream) = OscListener::bind(0).await.unwrap();
        let port = listener.local_addr().port();
        let sock = UdpSocket::bind(loopback(0)).await.unwrap();
        sock.send_to(b"\x00\x01garbage", loopback(port))
            .await
            .unwrap();
        // A valid message after the garbage must still arrive.
        let client = OscClient::bind(loopback(port)).await.unwrap();
        client
            .send(&OscMessage {
                addr: "/ok".to_string(),
                args: vec![],
            })
            .await
            .unwrap();
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("timed out")
            .unwrap();
        assert_eq!(msg.addr, "/ok");
    }
}
