use super::*;
use std::fs;

#[test]
fn collect_dat_files_finds_dat_and_xml_recursively_and_ignores_others() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let root = dir.path();

    fs::write(root.join("a.dat"), "x").expect("write a.dat");
    fs::write(root.join("b.DAT"), "x").expect("write b.DAT"); // case-insensitive
    fs::create_dir(root.join("nested")).expect("mkdir nested");
    fs::write(root.join("nested/c.xml"), "x").expect("write c.xml");
    fs::write(root.join("notes.txt"), "x").expect("write notes.txt"); // ignored
    fs::write(root.join("archive.zip"), "x").expect("write archive.zip"); // ignored

    let found = collect_dat_files(root);
    let names: Vec<String> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    assert_eq!(found.len(), 3, "expected 3 DAT/XML files, got {names:?}");
    assert!(names.contains(&"a.dat".to_string()));
    assert!(names.contains(&"b.DAT".to_string()));
    assert!(names.contains(&"c.xml".to_string()));
    assert!(!names.iter().any(|n| n == "notes.txt" || n == "archive.zip"));
}

#[test]
fn collect_dat_files_on_empty_dir_returns_nothing() {
    let dir = tempfile::tempdir().expect("create temp dir");
    assert!(collect_dat_files(dir.path()).is_empty());
}

const MINIMAL_DAT: &str = r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>Test Collection</name>
    <version>2020-01-01</version>
  </header>
  <game name="Game One">
    <description>Game One</description>
    <rom name="game one.rom" size="1000" sha1="ABC123"/>
  </game>
</datafile>"#;

#[test]
fn import_dat_file_is_idempotent() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let dat_path = dir.path().join("Test Collection.dat");
    fs::write(&dat_path, MINIMAL_DAT).expect("write dat");

    let db = Database::open_in_memory().expect("open db");

    // First import adds the version.
    let first = import_dat_file(&db, &dat_path, None, true, None).expect("first import");
    assert!(
        matches!(first, ImportOutcome::Added { games: 1, .. }),
        "first import should add one game"
    );

    // Re-importing the same version is a reported no-op, not a UNIQUE error.
    let second = import_dat_file(&db, &dat_path, None, true, None).expect("second import");
    assert!(
        matches!(second, ImportOutcome::AlreadyPresent),
        "re-import should skip as already present"
    );

    // And no duplicate version row was created.
    let conn = db.conn();
    let coll = collections::get_collection_by_name(conn, "Test Collection")
        .expect("query collection")
        .expect("collection exists");
    assert_eq!(
        collections::count_versions(conn, coll.id).expect("count versions"),
        1,
        "exactly one version should exist after a repeated import"
    );
}

#[test]
fn sort_segments_splits_collection_name_on_dash() {
    assert_eq!(
        sort_segments("Acorn BBC - Magazines - Laserbug"),
        vec!["Acorn BBC", "Magazines", "Laserbug"]
    );
    assert_eq!(sort_segments("Sony - Books"), vec!["Sony", "Books"]);
    // No " - " → a single segment.
    assert_eq!(sort_segments("MAME 0.261"), vec!["MAME 0.261"]);
    // Path separators in a segment are neutralised.
    assert_eq!(sort_segments("A/B - C"), vec!["A_B", "C"]);
}

#[test]
fn relative_hierarchy_derives_nested_path() {
    let root = Path::new("/dats/TOSEC-PIX");
    let file = Path::new("/dats/TOSEC-PIX/Acorn/BBC/Magazines/Laserbug/x.dat");
    assert_eq!(
        relative_hierarchy(file, root),
        Some("Acorn/BBC/Magazines/Laserbug".to_string())
    );
}

#[test]
fn relative_hierarchy_is_none_at_root() {
    let root = Path::new("/dats/TOSEC-PIX");
    let file = Path::new("/dats/TOSEC-PIX/flat.dat");
    assert_eq!(relative_hierarchy(file, root), None);
}

#[test]
fn relative_hierarchy_is_none_when_unrelated_to_root() {
    let root = Path::new("/dats/TOSEC-PIX");
    let file = Path::new("/elsewhere/x.dat");
    assert_eq!(relative_hierarchy(file, root), None);
}

/// Read the single DAT node's stored `path` for a collection version.
fn node_path_for(db: &Database, collection: &str) -> String {
    let conn = db.conn();
    conn.query_row(
        "SELECT n.path FROM dat_nodes n
         JOIN collection_versions cv ON n.version_id = cv.id
         JOIN collections c ON cv.collection_id = c.id
         WHERE c.name = ?",
        [collection],
        |row| row.get::<_, String>(0),
    )
    .expect("query node path")
}

#[test]
fn recursive_add_records_relative_hierarchy_on_the_node() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let nested = dir.path().join("Acorn/BBC/Magazines/Laserbug");
    fs::create_dir_all(&nested).expect("mkdir nested");
    fs::write(nested.join("coll.dat"), MINIMAL_DAT).expect("write dat");

    let db = Database::open_in_memory().expect("open db");
    let file = nested.join("coll.dat");
    let rel = relative_hierarchy(&file, dir.path());
    import_dat_file(&db, &file, None, true, rel.as_deref()).expect("import");

    assert_eq!(
        node_path_for(&db, "Test Collection"),
        "Acorn/BBC/Magazines/Laserbug",
        "the node path should carry the directory relative to the add root"
    );
}

#[test]
fn single_add_node_path_falls_back_to_collection_name() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let dat_path = dir.path().join("coll.dat");
    fs::write(&dat_path, MINIMAL_DAT).expect("write dat");

    let db = Database::open_in_memory().expect("open db");
    import_dat_file(&db, &dat_path, None, true, None).expect("import");

    assert_eq!(
        node_path_for(&db, "Test Collection"),
        "Test Collection",
        "with no hierarchy the node path falls back to the flat collection name"
    );
}
