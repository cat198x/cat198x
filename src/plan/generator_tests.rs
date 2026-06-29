use super::*;
use crate::db::files::{self, Disposition};
use crate::db::{Database, config as db_config, dats};
use crate::plan::OperationKind;
use rusqlite::params;
use std::collections::BTreeMap;

fn setup_db() -> Database {
    Database::open_in_memory().unwrap()
}

fn add_collection_node(
    conn: &Connection,
    collection_name: &str,
    source_type: &str,
    dat_path: &str,
    hierarchy: &str,
) -> i64 {
    let coll = collections::create_collection(conn, collection_name, source_type).unwrap();
    let version = collections::add_version(conn, coll, "v1", dat_path, true).unwrap();
    dats::create_node(conn, version, None, collection_name, "dat", hierarchy).unwrap()
}

fn add_game(conn: &Connection, node: i64, game_name: &str) -> i64 {
    dats::create_game(conn, node, game_name, None, None, false, false, false).unwrap()
}

fn add_rom_to_game(conn: &Connection, game: i64, rom_name: &str, sha1: &str) {
    dats::create_rom(
        conn,
        game,
        rom_name,
        10,
        Some(sha1),
        None,
        None,
        "good",
        None,
    )
    .unwrap();
}

fn add_rom(conn: &Connection, node: i64, game_name: &str, rom_name: &str, sha1: &str) {
    let game = add_game(conn, node, game_name);
    add_rom_to_game(conn, game, rom_name, sha1);
}

fn add_file(conn: &Connection, sha1: &str, size: i64) {
    conn.execute(
        "INSERT INTO files (sha1, size) VALUES (?1, ?2)",
        params![sha1, size],
    )
    .unwrap();
}

fn add_source(conn: &Connection, id: i64, path: &str, disposition: Option<Disposition>) {
    match disposition {
        Some(disposition) => conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition) VALUES (?1, ?2, 0, ?3)",
            params![id, path, disposition.as_str()],
        ),
        None => conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (?1, ?2, 0)",
            params![id, path],
        ),
    }
    .unwrap();
}

fn add_location(
    conn: &Connection,
    sha1: &str,
    source_id: i64,
    path: &str,
    archive_path: Option<&str>,
) {
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path)
         VALUES (?1, ?2, ?3, ?4)",
        params![sha1, source_id, path, archive_path],
    )
    .unwrap();
}

fn plan_with_default_dest(conn: &Connection, default_format: OutputFormat) -> Plan {
    generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format,
            ..Default::default()
        },
    )
    .unwrap()
}

/// Build a one-ROM collection whose held file exists in two places: already
/// at its canonical destination under the library, and a staged duplicate
/// elsewhere. `archived` controls whether the file is a loose file or an
/// inner entry of a `.zip` (and sets the per-set format accordingly).
fn setup_dup_fixture(conn: &Connection, archived: bool) {
    let node = add_collection_node(conn, "Test Coll", "tosec", "/dats/test.dat", "SET/Sys");
    add_rom(conn, node, "Game", "game.rom", "AAA");
    add_file(conn, "AAA", 10);

    // Library copy (already at the canonical destination) and a ToSort dup.
    add_source(conn, 101, "/lib/ROMs/SET/Sys", Some(Disposition::Preserve));
    add_source(conn, 102, "/lib/ToSort/SET", Some(Disposition::Consume));
    if archived {
        // Each copy is a .zip holding the ROM as an inner entry.
        add_location(conn, "AAA", 101, "Game.zip", Some("game.rom"));
        add_location(conn, "AAA", 102, "Sys/Game.zip", Some("game.rom"));
        db_config::set_output_format(conn, "SET", "zip").unwrap();
    } else {
        add_location(conn, "AAA", 101, "game.rom", None);
        add_location(conn, "AAA", 102, "Sys/game.rom", None);
    }
}

#[path = "generator_tests/archive_repack_policy.rs"]
mod archive_repack_policy;
#[path = "generator_tests/duplicate_delete_policy.rs"]
mod duplicate_delete_policy;
#[path = "generator_tests/planning_scope.rs"]
mod planning_scope;
