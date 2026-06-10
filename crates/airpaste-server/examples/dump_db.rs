//! Debug helper: dump devices and pairing sessions from a (copy of a) server redb file.
//! Usage: cargo run -p airpaste-server --example dump_db -- <path-to-redb>

use airpaste_core::{Device, PairingSession};
use redb::{Database, ReadableTable, TableDefinition};

const DEVICES: TableDefinition<&str, &[u8]> = TableDefinition::new("devices");
const PAIRINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("pairing_sessions");

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: dump_db <path>");
    let db = Database::open(&path)?;
    let txn = db.begin_read()?;

    println!("== devices ==");
    let table = txn.open_table(DEVICES)?;
    for entry in table.iter()? {
        let (_, value) = entry?;
        let device: Device = serde_json::from_slice(value.value())?;
        println!(
            "{}  name={:?}  trusted={}  created_at={}  last_seen={:?}",
            device.device_id.as_str(),
            device.name,
            device.trusted,
            device.created_at,
            device.last_seen_at
        );
    }

    println!("\n== pairing sessions ==");
    let table = txn.open_table(PAIRINGS)?;
    for entry in table.iter()? {
        let (key, value) = entry?;
        let session: PairingSession = serde_json::from_slice(value.value())?;
        println!(
            "code={}  created_by={:?}  candidate={:?}  confirmed={}  expires_at={}",
            key.value(),
            session.created_by.as_ref().map(|id| id.as_str()),
            session.candidate_device_id.as_ref().map(|id| id.as_str()),
            session.confirmed,
            session.expires_at
        );
    }
    Ok(())
}
