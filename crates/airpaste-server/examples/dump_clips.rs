//! Debug helper: dump clip records from a (copy of a) server redb file.
use redb::{Database, ReadableTable, TableDefinition};

const CLIPS: TableDefinition<&str, &[u8]> = TableDefinition::new("clip_records");

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: dump_clips <path>");
    let db = Database::open(&path)?;
    let txn = db.begin_read()?;
    let table = txn.open_table(CLIPS)?;
    let mut rows: Vec<(String, serde_json::Value)> = Vec::new();
    for entry in table.iter()? {
        let (key, value) = entry?;
        let v: serde_json::Value = serde_json::from_slice(value.value())?;
        rows.push((key.value().to_string(), v));
    }
    rows.sort_by(|a, b| {
        a.1["created_at"]
            .as_str()
            .unwrap_or("")
            .cmp(b.1["created_at"].as_str().unwrap_or(""))
    });
    for (key, v) in rows {
        let kind = if v["kind"]["Text"].is_object() {
            format!("text(len={})", v["kind"]["Text"]["utf8_len"])
        } else if v["kind"]["Files"].is_object() {
            "files".to_string()
        } else {
            format!("{}", v["kind"])
        };
        println!(
            "{}  created_at={}  source={}  kind={}  scheme={}",
            key,
            v["created_at"].as_str().unwrap_or("?"),
            v["source_device_id"].as_str().unwrap_or("?"),
            kind,
            v["encryption"]["scheme"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}
