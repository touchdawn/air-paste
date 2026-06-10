//! Debug helper: mark a registered device as trusted directly in a server redb file.
//! The server must not be running (redb holds an exclusive lock).
//! Usage: cargo run -p airpaste-server --example trust_device -- <path-to-redb> <device-id>

use airpaste_core::Device;
use redb::{Database, ReadableTable, TableDefinition};

const DEVICES: TableDefinition<&str, &[u8]> = TableDefinition::new("devices");

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: trust_device <path> <device-id>");
    let device_id = args.next().expect("usage: trust_device <path> <device-id>");

    let db = Database::open(&path)?;
    let txn = db.begin_write()?;
    {
        let mut table = txn.open_table(DEVICES)?;
        let mut device: Device = {
            let value = table
                .get(device_id.as_str())?
                .ok_or_else(|| anyhow::anyhow!("device {device_id} not found"))?;
            serde_json::from_slice(value.value())?
        };
        println!(
            "before: {} name={:?} trusted={}",
            device.device_id.as_str(),
            device.name,
            device.trusted
        );
        device.trusted = true;
        let body = serde_json::to_vec(&device)?;
        table.insert(device_id.as_str(), body.as_slice())?;
        println!("after:  trusted=true");
    }
    txn.commit()?;
    Ok(())
}
