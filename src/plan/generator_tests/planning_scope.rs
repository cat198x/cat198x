use super::*;

#[test]
fn test_generate_plan_no_config() {
    let db = setup_db();
    let conn = db.conn();

    let plan = generate_plan(conn).unwrap();
    assert!(plan.is_empty());
}

#[test]
fn plan_records_collections_skipped_for_no_destination() {
    let db = setup_db();
    let conn = db.conn();

    // A collection with an active version but no dest_path and no default.
    let cid = collections::create_collection(conn, "No Dest Coll", "tosec").unwrap();
    let vid = collections::add_version(conn, cid, "1.0", "/tmp/x.dat", true).unwrap();
    dats::create_node(conn, vid, None, "No Dest Coll", "dat", "No Dest Coll").unwrap();

    let plan = generate_plan_filtered(conn, &PlanOptions::default()).unwrap();
    assert!(plan.is_empty(), "no destination → no operations");
    assert_eq!(plan.skipped_no_dest, vec!["No Dest Coll".to_string()]);
}

#[test]
fn refuses_when_two_collections_share_a_destination_root() {
    let db = setup_db();
    let conn = db.conn();
    // Both collections have the flat hierarchy "FBN", so both resolve to
    // <default>/FBN — the flat-namespace trap that overwrites same-named games.
    for name in ["Arcade Games", "Game Gear Games"] {
        let c = collections::create_collection(conn, name, "mame").unwrap();
        let vid = collections::add_version(conn, c, "v1", &format!("/d/{name}.dat"), true).unwrap();
        dats::create_node(conn, vid, None, name, "dat", "FBN").unwrap();
    }
    let err = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            ..Default::default()
        },
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("share a destination root"), "got: {msg}");
    assert!(
        msg.contains("/lib/ROMs/FBN"),
        "names the shared root: {msg}"
    );
    assert!(
        msg.contains("Arcade Games") && msg.contains("Game Gear Games"),
        "names the colliding collections: {msg}"
    );
}

#[test]
fn allows_collections_with_distinct_destination_roots() {
    let db = setup_db();
    let conn = db.conn();
    // Per-machine hierarchies → distinct roots → no collision, plan proceeds.
    for (name, path) in [
        ("Arcade Games", "FBN/Arcade"),
        ("Game Gear Games", "FBN/Game Gear"),
    ] {
        let c = collections::create_collection(conn, name, "mame").unwrap();
        let vid = collections::add_version(conn, c, "v1", &format!("/d/{name}.dat"), true).unwrap();
        dats::create_node(conn, vid, None, name, "dat", path).unwrap();
    }
    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        plan.is_empty(),
        "no held content, so an empty but valid plan"
    );
}

#[test]
fn allows_a_chd_collection_to_share_a_root_with_a_rom_collection() {
    let db = setup_db();
    let conn = db.conn();
    // A ROM collection and a disk-only CHD collection, both at root "Demul".
    // A game's `<game>.zip` and its `<game>/<name>.chd` don't collide.
    let rc = collections::create_collection(conn, "Demul ROMs", "mame").unwrap();
    let rv = collections::add_version(conn, rc, "v1", "/d/r.dat", true).unwrap();
    let rn = dats::create_node(conn, rv, None, "Demul ROMs", "dat", "Demul").unwrap();
    let rg = dats::create_game(conn, rn, "azumanga", None, None, false, false, false).unwrap();
    dats::create_rom(conn, rg, "a.rom", 10, Some("AAA"), None, None, "good", None).unwrap();

    let cc = collections::create_collection(conn, "Demul CHDs", "mame").unwrap();
    let cv = collections::add_version(conn, cc, "v1", "/d/c.dat", true).unwrap();
    let cn = dats::create_node(conn, cv, None, "Demul CHDs", "dat", "Demul").unwrap();
    let cg = dats::create_game(conn, cn, "azumanga", None, None, false, false, false).unwrap();
    dats::create_disk(conn, cg, "gdl-0018", Some("DDD"), None, "good", None).unwrap();

    // The guard must NOT refuse — ROM and CHD are different output namespaces.
    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(plan.is_empty(), "no held content, but an un-refused plan");
}

#[test]
fn set_filter_restricts_planning_to_requested_sets() {
    let db = setup_db();
    let conn = db.conn();
    setup_dup_fixture(conn, false); // collection whose set (top segment) is "SET"

    let opts = |sets: Option<Vec<String>>| PlanOptions {
        set_filter: sets,
        default_dest: Some("/lib/ROMs".to_string()),
        default_format: OutputFormat::Loose,
        ..Default::default()
    };

    // A non-matching set is skipped entirely — no operations.
    let other = generate_plan_filtered(conn, &opts(Some(vec!["TOSEC".to_string()]))).unwrap();
    assert!(
        other.is_empty(),
        "collection in set 'SET' excluded by --set TOSEC"
    );

    // The matching set is planned.
    let matched = generate_plan_filtered(conn, &opts(Some(vec!["SET".to_string()]))).unwrap();
    assert!(!matched.is_empty(), "set 'SET' is planned when requested");
}
