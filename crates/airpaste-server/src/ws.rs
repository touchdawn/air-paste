use crate::state::AppState;
use airpaste_core::DeviceId;
use airpaste_protocol::{ClientEvent, ServerEvent};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;

pub async fn ws_handler(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let Some(Ok(Message::Text(first))) = receiver.next().await else {
        return;
    };

    let Ok(ClientEvent::Hello { device_id }) = serde_json::from_str::<ClientEvent>(&first) else {
        let _ = sender
            .send(Message::Text(
                serde_json::to_string(&ServerEvent::Error {
                    message: "first websocket message must be hello".to_string(),
                })
                .unwrap(),
            ))
            .await;
        return;
    };

    let _ = state.store.touch_device(&device_id);
    state.hub.broadcast(ServerEvent::DeviceOnline {
        device_id: device_id.clone(),
    });

    if send_json(
        &mut sender,
        &ServerEvent::HelloAck {
            device_id: device_id.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }

    let mut global_rx = state.hub.subscribe_global();
    let mut device_rx = state.hub.subscribe_device(&device_id).await;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            maybe_msg = receiver.next() => {
                match maybe_msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(error) = handle_client_event(&state, &device_id, &text).await {
                            let _ = send_json(&mut sender, &ServerEvent::Error { message: error }).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) | Some(Ok(Message::Binary(_))) => {}
                    Some(Err(error)) => {
                        tracing::warn!(%device_id, %error, "websocket receive failed");
                        break;
                    }
                }
            }
            event = recv_event(&mut global_rx) => {
                if let Some(event) = event {
                    if send_json(&mut sender, &event).await.is_err() {
                        break;
                    }
                }
            }
            event = recv_event(&mut device_rx) => {
                if let Some(event) = event {
                    if send_json(&mut sender, &event).await.is_err() {
                        break;
                    }
                }
            }
            _ = heartbeat.tick() => {
                let _ = state.store.touch_device(&device_id);
            }
        }
    }

    state
        .hub
        .broadcast(ServerEvent::DeviceOffline { device_id });
}

async fn handle_client_event(
    state: &AppState,
    from_device_id: &DeviceId,
    text: &str,
) -> Result<(), String> {
    let event = serde_json::from_str::<ClientEvent>(text).map_err(|error| error.to_string())?;
    match event {
        ClientEvent::Hello { .. } | ClientEvent::Heartbeat => {
            let _ = state.store.touch_device(from_device_id);
        }
        ClientEvent::TransferOffer {
            to_device_id,
            payload,
        } => {
            state
                .hub
                .send_to(
                    &to_device_id,
                    ServerEvent::TransferOffer {
                        from_device_id: from_device_id.clone(),
                        payload,
                    },
                )
                .await;
        }
        ClientEvent::TransferAnswer {
            to_device_id,
            payload,
        } => {
            state
                .hub
                .send_to(
                    &to_device_id,
                    ServerEvent::TransferAnswer {
                        from_device_id: from_device_id.clone(),
                        payload,
                    },
                )
                .await;
        }
        ClientEvent::TransferCandidate {
            to_device_id,
            payload,
        } => {
            state
                .hub
                .send_to(
                    &to_device_id,
                    ServerEvent::TransferCandidate {
                        from_device_id: from_device_id.clone(),
                        payload,
                    },
                )
                .await;
        }
        ClientEvent::TransferCancelled {
            to_device_id,
            session_id,
            reason,
        } => {
            state
                .hub
                .send_to(
                    &to_device_id,
                    ServerEvent::TransferCancelled {
                        from_device_id: from_device_id.clone(),
                        session_id,
                        reason,
                    },
                )
                .await;
        }
    }
    Ok(())
}

async fn recv_event(rx: &mut broadcast::Receiver<ServerEvent>) -> Option<ServerEvent> {
    loop {
        match rx.recv().await {
            Ok(event) => return Some(event),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "websocket event receiver lagged");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

async fn send_json(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    event: &ServerEvent,
) -> Result<(), axum::Error> {
    let text = serde_json::to_string(event).expect("server event must serialize");
    sender.send(Message::Text(text)).await
}
