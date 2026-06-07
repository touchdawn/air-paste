//! Server-mediated relay data path.
//!
//! The relay is a dumb, in-memory pipe between the two devices of an authorized relay
//! session. The server forwards opaque frames from one peer to the other, enforces the
//! session byte budget, and never inspects or persists payloads — the agents end-to-end
//! encrypt the bytes before they reach the server.

use airpaste_core::SessionId;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{mpsc, Mutex};

/// Max frames buffered per direction before the forwarding reader applies backpressure to its
/// socket. Bounds the relay's in-memory footprint (≈ this many relay chunks per direction).
const RELAY_QUEUE_CAPACITY: usize = 64;

/// Which end of a relay session a connection represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayRole {
    Source,
    Recipient,
}

/// A frame forwarded verbatim between the two peers of a session.
#[derive(Clone)]
enum RelayFrame {
    Text(String),
    Binary(Vec<u8>),
}

/// Per-connection endpoints: receive frames destined for this connection, and a sender
/// that pushes frames to the peer connection.
struct RelayEnds {
    outbound_rx: mpsc::Receiver<RelayFrame>,
    peer_tx: mpsc::Sender<RelayFrame>,
}

struct RelayRoom {
    to_source: mpsc::Sender<RelayFrame>,
    to_recipient: mpsc::Sender<RelayFrame>,
    source_rx: Option<mpsc::Receiver<RelayFrame>>,
    recipient_rx: Option<mpsc::Receiver<RelayFrame>>,
}

#[derive(Clone, Default)]
pub struct RelayHub {
    rooms: Arc<Mutex<HashMap<SessionId, RelayRoom>>>,
}

impl RelayHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Join a session as `role`. Returns `None` if that role is already connected.
    async fn join(&self, session_id: &SessionId, role: RelayRole) -> Option<RelayEnds> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms.entry(session_id.clone()).or_insert_with(|| {
            let (to_source, source_rx) = mpsc::channel(RELAY_QUEUE_CAPACITY);
            let (to_recipient, recipient_rx) = mpsc::channel(RELAY_QUEUE_CAPACITY);
            RelayRoom {
                to_source,
                to_recipient,
                source_rx: Some(source_rx),
                recipient_rx: Some(recipient_rx),
            }
        });
        match role {
            RelayRole::Source => Some(RelayEnds {
                outbound_rx: room.source_rx.take()?,
                peer_tx: room.to_recipient.clone(),
            }),
            RelayRole::Recipient => Some(RelayEnds {
                outbound_rx: room.recipient_rx.take()?,
                peer_tx: room.to_source.clone(),
            }),
        }
    }

    async fn leave(&self, session_id: &SessionId) {
        self.rooms.lock().await.remove(session_id);
    }
}

/// Drive one relay websocket connection: forward this peer's frames to the other peer, and
/// deliver the other peer's frames to this socket, until either side closes, the session byte
/// budget is exhausted, or the session TTL elapses. Reading and writing run as independent
/// tasks so a bounded (backpressured) forward queue cannot deadlock the reverse direction.
pub async fn relay_ws_handler(
    socket: WebSocket,
    hub: RelayHub,
    session_id: SessionId,
    role: RelayRole,
    max_bytes: u64,
    ttl: Duration,
) {
    let Some(ends) = hub.join(&session_id, role).await else {
        tracing::warn!(%session_id, ?role, "relay role already connected");
        return;
    };
    tracing::info!(%session_id, ?role, "relay peer connected");

    let (mut sink, mut stream) = socket.split();
    let RelayEnds {
        mut outbound_rx,
        peer_tx,
    } = ends;

    // Reader: this socket -> peer's bounded queue. A full queue backpressures this socket.
    let read_session = session_id.clone();
    let mut reader = tokio::spawn(async move {
        let mut forwarded: u64 = 0;
        while let Some(message) = stream.next().await {
            let frame = match message {
                Ok(Message::Text(text)) => {
                    forwarded += text.len() as u64;
                    RelayFrame::Text(text)
                }
                Ok(Message::Binary(bytes)) => {
                    forwarded += bytes.len() as u64;
                    RelayFrame::Binary(bytes)
                }
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(%read_session, ?role, %error, "relay receive failed");
                    break;
                }
            };
            if forwarded > max_bytes {
                tracing::warn!(%read_session, ?role, "relay byte budget exceeded");
                break;
            }
            if peer_tx.send(frame).await.is_err() {
                break;
            }
        }
    });

    // Writer: peer frames -> this socket.
    let mut writer = tokio::spawn(async move {
        while let Some(frame) = outbound_rx.recv().await {
            let message = match frame {
                RelayFrame::Text(text) => Message::Text(text),
                RelayFrame::Binary(bytes) => Message::Binary(bytes),
            };
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    let ttl_timer = tokio::time::sleep(ttl);
    tokio::pin!(ttl_timer);
    tokio::select! {
        _ = &mut reader => {}
        _ = &mut writer => {}
        _ = &mut ttl_timer => {
            tracing::info!(%session_id, ?role, "relay session ttl reached; closing");
        }
    }
    reader.abort();
    writer.abort();

    tracing::info!(%session_id, ?role, "relay peer disconnected");
    hub.leave(&session_id).await;
}
