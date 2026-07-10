use anyhow::Result;
use rusqlite::Connection;
use sha2::{Digest as Sha2Digest, Sha256};

use crate::db::{collections, config as db_config};

/// Compute state hash for plan validation.
pub fn compute_state_hash(conn: &Connection) -> Result<String> {
    let mut hasher = Sha256::new();

    // 1. Active version IDs (sorted)
    let mut active_ids: Vec<i64> = Vec::new();
    let colls = collections::list_collections(conn)?;
    for coll in &colls {
        if let Some(ver) = collections::get_active_version(conn, coll.id)? {
            active_ids.push(ver.id);
        }
    }
    active_ids.sort();
    for id in &active_ids {
        hasher.update(id.to_le_bytes());
    }

    // 2. File catalog fingerprint (row count + max last_seen)
    let (file_count, max_last_seen): (i64, Option<String>) = conn.query_row(
        "SELECT COUNT(*), MAX(last_seen) FROM file_locations",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    hasher.update(file_count.to_le_bytes());
    if let Some(ts) = max_last_seen {
        hasher.update(ts.as_bytes());
    }

    // 3. Destination config hash
    let configs = db_config::list_all_configs(conn)?;
    for cfg in &configs {
        hasher.update(cfg.path_pattern.as_bytes());
        if let Some(ref dest) = cfg.dest_path {
            hasher.update(dest.as_bytes());
        }
    }

    let result = hasher.finalize();
    Ok(crate::util::hex_lower(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn compute_state_hash_empty() {
        let db = Database::open_in_memory().unwrap();
        let hash = compute_state_hash(db.conn()).unwrap();

        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 hex = 64 chars
    }

    #[test]
    fn compute_state_hash_deterministic() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        let hash1 = compute_state_hash(conn).unwrap();
        let hash2 = compute_state_hash(conn).unwrap();
        assert_eq!(hash1, hash2);
    }
}
