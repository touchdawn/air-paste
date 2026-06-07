//! Server-mediated relay data path.
//!
//! The relay is a dumb, in-memory pipe between the two devices of an authorized relay
//! session. The server forwards opaque frames from one peer to the other, enforces the
//! session byte budget, and never inspects or persists payloads — the agents end-to-end
//! encrypt the bytes before they reach the server.

use airpaste_core::SessionId;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, Mutex};

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
    outbound_rx: mpsc::UnboundedReceiver<RelayFrame>,
    peer_tx: mpsc::UnboundedSender<RelayFrame>,
}

struct RelayRoom {
    to_source: mpsc::UnboundedSender<RelayFrame>,
    to_recipient: mpsc::UnboundedSender<RelayFrame>,
    source_rx: Option<mpsc::UnboundedReceiver<RelayFrame>>,
    recipient_rx: Option<mpsc::UnboundedReceiver<RelayFrame>>,
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
            let (to_source, source_rx) = mpsc::unbounded_channel();
            let (to_recipient, recipient_rx) = mpsc::unbounded_channel();
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

/// Drive one relay websocket connection: forward this peer's frames to the other peer,
/// and deliver the other peer's frames to this socket, until either side closes or the
/// session byte budget is exhausted.
pub async fn relay_ws_handler(
    socket: WebSocket,
    hub: RelayHub,
    session_id: SessionId,
    role: RelayRole,
    max_bytes: u64,
) {
    let Some(mut ends) = hub.join(&session_id, role).await else {
        tracing::warn!(%session_id, ?role, "relay role already connected");
        return;
    };
    tracing::info!(%session_id, ?role, "relay peer connected");

    let (mut sink, mut stream) = socket.split();
    let mut forwarded: u64 = 0;

    loop {
        tokio::select! {
            inbound = stream.next() => {
                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        forwarded += text.len() as u64;
                        if forwarded > max_bytes {
                            tracing::warn!(%session_id, ?role, "relay byte budget exceeded");
                            break;
                        }
                        if ends.peer_tx.send(RelayFrame::Text(text)).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        forwarded += bytes.len() as u64;
                        if forwarded > max_bytes {
                            tracing::warn!(%session_id, ?role, "relay byte budget exceeded");
                            break;
                        }
                        if ends.peer_tx.send(RelayFrame::Binary(bytes)).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        tracing::warn!(%session_id, ?role, %error, "relay receive failed");
                        break;
                    }
                }
            }
            outbound = ends.outbound_rx.recv() => {
                let Some(frame) = outbound else { break };
                let message = match frame {
                    RelayFrame::Text(text) => Message::Text(text),
                    RelayFrame::Binary(bytes) => Message::Binary(bytes),
                };
                if sink.send(message).await.is_err() {
                    break;
                }
            }
        }
    }

    tracing::info!(%session_id, ?role, "relay peer disconnected");
    hub.leave(&session_id).await;
}
