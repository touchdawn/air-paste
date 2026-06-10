//! LAN peer discovery over mDNS (`_airpaste._tcp.local.`).
//!
//! Each agent advertises its peer file server and browses for other agents, keeping a
//! directory of `device_id -> LAN socket address`. Receivers prefer a discovered address
//! over the manifest's `source_peer_url`, so direct LAN transfer works without manually
//! configuring `--peer-public-url`.

use airpaste_core::DeviceId;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
use tokio::sync::RwLock;

const SERVICE_TYPE: &str = "_airpaste._tcp.local.";
const TXT_DEVICE_ID: &str = "device_id";
const TXT_DEVICE_NAME: &str = "device_name";

/// Live map of peer `device_id` to its most recently resolved LAN socket address.
#[derive(Clone, Default)]
pub struct PeerDirectory {
    peers: Arc<RwLock<HashMap<DeviceId, SocketAddr>>>,
}

impl PeerDirectory {
    pub async fn resolve(&self, device_id: &DeviceId) -> Option<SocketAddr> {
        self.peers.read().await.get(device_id).copied()
    }

    async fn upsert(&self, device_id: DeviceId, addr: SocketAddr) {
        self.peers.write().await.insert(device_id, addr);
    }
}

/// Advertise this device's peer service and start browsing for peers.
///
/// Returns the daemon (keep it alive for the process lifetime) and the shared directory.
/// Discovery failures are surfaced to the caller, which may continue without mDNS.
pub fn start(
    device_id: &DeviceId,
    device_name: &str,
    peer_port: u16,
) -> anyhow::Result<(ServiceDaemon, PeerDirectory)> {
    let daemon = ServiceDaemon::new()?;

    let instance = sanitize_instance(device_id.as_str());
    let host_name = format!("{instance}.local.");
    let mut properties = HashMap::new();
    properties.insert(TXT_DEVICE_ID.to_string(), device_id.as_str().to_string());
    properties.insert(TXT_DEVICE_NAME.to_string(), device_name.to_string());

    // Empty address + enable_addr_auto lets mdns-sd announce all reachable interface IPs.
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &instance,
        &host_name,
        "",
        peer_port,
        properties,
    )?
    .enable_addr_auto();
    daemon.register(service)?;

    let directory = PeerDirectory::default();
    let events = daemon.browse(SERVICE_TYPE)?;
    let directory_for_task = directory.clone();
    let own_device_id = device_id.clone();

    tokio::spawn(async move {
        while let Ok(event) = events.recv_async().await {
            if let ServiceEvent::ServiceResolved(info) = event {
                let Some(peer_id) = info.get_property_val_str(TXT_DEVICE_ID) else {
                    continue;
                };
                let peer_id = DeviceId(peer_id.to_string());
                if peer_id == own_device_id {
                    continue;
                }
                if let Some(addr) = pick_lan_addr(&info) {
                    tracing::info!(device_id = %peer_id, %addr, "discovered peer via mDNS");
                    directory_for_task.upsert(peer_id, addr).await;
                }
            }
        }
        tracing::warn!("mDNS browse channel closed");
    });

    tracing::info!(%device_id, peer_port, "advertising peer service over mDNS");
    Ok((daemon, directory))
}

/// Prefer a private (LAN) IPv4 address; otherwise fall back to any non-loopback IPv4.
fn pick_lan_addr(info: &ServiceInfo) -> Option<SocketAddr> {
    let port = info.get_port();
    let mut fallback: Option<IpAddr> = None;
    for ip in info.get_addresses() {
        let IpAddr::V4(v4) = ip else {
            continue;
        };
        if v4.is_loopback() || v4.is_link_local() {
            continue;
        }
        if v4.is_private() {
            return Some(SocketAddr::new(*ip, port));
        }
        fallback.get_or_insert(*ip);
    }
    fallback.map(|ip| SocketAddr::new(ip, port))
}

fn sanitize_instance(device_id: &str) -> String {
    device_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
