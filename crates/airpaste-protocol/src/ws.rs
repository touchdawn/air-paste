use airpaste_core::{ClipId, DeviceId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    Hello {
        device_id: DeviceId,
    },
    Heartbeat,
    TransferOffer {
        to_device_id: DeviceId,
        payload: Value,
    },
    TransferAnswer {
        to_device_id: DeviceId,
        payload: Value,
    },
    TransferCandidate {
        to_device_id: DeviceId,
        payload: Value,
    },
    TransferCancelled {
        to_device_id: DeviceId,
        session_id: Option<SessionId>,
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    HelloAck {
        device_id: DeviceId,
    },
    DeviceOnline {
        device_id: DeviceId,
    },
    DeviceOffline {
        device_id: DeviceId,
    },
    ClipCreated {
        clip_id: ClipId,
        source_device_id: DeviceId,
        kind: String,
    },
    TransferOffer {
        from_device_id: DeviceId,
        payload: Value,
    },
    TransferAnswer {
        from_device_id: DeviceId,
        payload: Value,
    },
    TransferCandidate {
        from_device_id: DeviceId,
        payload: Value,
    },
    TransferCancelled {
        from_device_id: DeviceId,
        session_id: Option<SessionId>,
        reason: Option<String>,
    },
    TransferRelayReady {
        session_id: SessionId,
        clip_id: ClipId,
        source_device_id: DeviceId,
        recipient_device_id: DeviceId,
    },
    Error {
        message: String,
    },
}
