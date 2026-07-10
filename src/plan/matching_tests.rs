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
    assert_eq!(count_expansion_capped(conn, vid, 100, None).unwrap(), 6);
    // A cap below the expansion is detected without counting past cap + 1.
    let capped = count_expansion_capped(conn, vid, 4, None).unwrap();
    assert_eq!(capped, 5, "the inner LIMIT halts at cap + 1");
    assert!(capped > 4, "over-cap is reported as exceeding the cap");
}

/// Build the icons pathology in miniature: one content (a "blank icon")
/// referenced by `roms` DAT entries and physically held in `holders` places,
/// so the uncapped expansion is `roms x holders` -- plus one *rare* content
/// (a unique icon) held once. Returns the version id.
fn setup_blank_icon_blowup(conn: &rusqlite::Connection, roms: usize, holders: usize) -> i64 {
    let coll = collections::create_collection(conn, "Icons", "mame").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/i.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Icons", "dat", "MAME").unwrap();
    let g = dats::create_game(conn, node, "icons", None, None, false, false, false).unwrap();
    // `roms` machines all ship the identical blank icon (content BLANK)...
    for i in 0..roms {
        dats::create_rom(
            conn,
            g,
            &format!("m{i}.ico"),
            6,
            Some("BLANK"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
    }
    // ...plus one machine with a genuinely unique icon (content RARE).
    dats::create_rom(
        conn,
        g,
        "unique.ico",
        6,
        Some("RARE"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files (sha1, size) VALUES ('BLANK', 6), ('RARE', 6)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive) VALUES (700, '/lib', 0)",
        [],
    )
    .unwrap();
    // The blank icon sits in `holders` places; the rare icon in exactly one.
    for i in 0..holders {
        conn.execute(
            &format!(
                "INSERT INTO file_locations (sha1, source_id, path) VALUES ('BLANK', 700, 'b{i}.ico')"
            ),
            [],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path) VALUES ('RARE', 700, 'unique.ico')",
        [],
    )
    .unwrap();
    vid
}

#[test]
fn location_cap_bounds_the_cross_product_so_an_over_budget_set_becomes_plannable() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    // 8 machines share the blank icon, held in 8 places: 8 x 8 = 64 uncapped
    // rows from one content (the real pathology in miniature), + 1 rare row.
    let vid = setup_blank_icon_blowup(conn, 8, 8);

    // Uncapped, the expansion is the full cross-product (64 blank + 1 rare).
    assert_eq!(count_expansion_capped(conn, vid, 1000, None).unwrap(), 65);

    // Bounded to 2 holders per content, the blank icon contributes only
    // 8 roms x 2 = 16, plus the untouched rare row: 17.
    assert_eq!(
        count_expansion_capped(conn, vid, 1000, Some(2)).unwrap(),
        17,
        "the per-content cap collapses the duplicate cross-product"
    );

    // The planner's decision in miniature: with a row budget of 40, the set is
    // over budget uncapped (65 > 40) but fits once bounded (17 <= 40) -- exactly
    // the path that turns a skipped collection into a planned one.
    assert!(count_expansion_capped(conn, vid, 40, None).unwrap() > 40);
    assert!(count_expansion_capped(conn, vid, 40, Some(2)).unwrap() <= 40);
}

#[test]
fn location_cap_bounds_duplicated_content_but_leaves_rare_content_whole() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    // Blank icon held in 10 places; rare icon in 1.
    let vid = setup_blank_icon_blowup(conn, 3, 10);

    // Uncapped: 3 roms x 10 holders of BLANK + 1 RARE = 31 rows.
    let uncapped = find_matched_roms(conn, vid, "Icons", false, None).unwrap();
    assert_eq!(uncapped.len(), 31);

    // Capped at 4 holders/content: BLANK gives 3 x 4 = 12, RARE still 1 = 13.
    let capped = find_matched_roms(conn, vid, "Icons", false, Some(4)).unwrap();
    assert_eq!(capped.len(), 13);

    // The rare content is never bounded -- every reference to it survives, so
    // build-from / completeness decisions that key off rare ROMs are intact.
    assert_eq!(
        capped.iter().filter(|m| m.sha1 == "RARE").count(),
        1,
        "rare content (held once) is untouched by the cap"
    );
    // The blank content is bounded to the cap per the ROMs referencing it.
    assert_eq!(capped.iter().filter(|m| m.sha1 == "BLANK").count(), 12);
}
