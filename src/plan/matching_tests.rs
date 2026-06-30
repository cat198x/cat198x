use super::*;
use crate::db::{Database, collections, dats};

#[test]
fn count_expansion_capped_caps_and_counts_with_rom_multiplicity() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    // Two distinct ROMs share content AAA, which is held in three locations.
    // The materialised expansion is one row per (matched ROM x location) =
    // 2 ROMs x 3 locations = 6.
    let coll = collections::create_collection(conn, "Agg", "mame").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/agg.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Agg", "dat", "MAME").unwrap();
    let g = dats::create_game(conn, node, "bucket", None, None, false, false, false).unwrap();
    dats::create_rom(conn, g, "a.rom", 10, Some("AAA"), None, None, "good", None).unwrap();
    dats::create_rom(conn, g, "b.rom", 10, Some("AAA"), None, None, "good", None).unwrap();
    conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
        .unwrap();
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive) VALUES (500, '/src', 0)",
        [],
    )
    .unwrap();
    for i in 0..3 {
        conn.execute(
            &format!(
                "INSERT INTO file_locations (sha1, source_id, path, archive_path)
                     VALUES ('AAA', 500, 'loc{i}.zip', 'x.rom')"
            ),
            [],
        )
        .unwrap();
    }

    // A generous cap returns the true expansion (6).
    assert_eq!(count_expansion_capped(conn, vid, 100).unwrap(), 6);
    // A cap below the expansion is detected without counting past cap + 1.
    let capped = count_expansion_capped(conn, vid, 4).unwrap();
    assert_eq!(capped, 5, "the inner LIMIT halts at cap + 1");
    assert!(capped > 4, "over-cap is reported as exceeding the cap");
}
