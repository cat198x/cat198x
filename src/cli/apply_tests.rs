use crate::db::Database;
use crate::db::files::{
    Disposition, add_source, get_source_by_path, set_source_disposition, upsert_file,
    upsert_file_location,
};
use crate::plan::executor::delete_has_surviving_copy;

// A delete is allowed only while a surviving copy physically exists; once the
// other copy is gone, the same record must no longer authorise the delete.
// The staging source is `consume`, so a copy in the library (another tree)
// does authorise emptying it.
#[test]
fn delete_refused_when_no_surviving_copy_on_disk() {
    let tosort = tempfile::TempDir::new().unwrap();
    let library = tempfile::TempDir::new().unwrap();
    let tosort_root = tosort.path().to_str().unwrap();
    let library_root = library.path().to_str().unwrap();

    // The same content exists physically in both the staging source and the
    // library, and is catalogued in both.
    std::fs::write(tosort.path().join("game.zip"), b"content").unwrap();
    std::fs::write(library.path().join("game.zip"), b"content").unwrap();

    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    add_source(conn, tosort_root, false).unwrap();
    add_source(conn, library_root, false).unwrap();
    // ToSort is staging — consume — so a cross-tree library copy counts.
    set_source_disposition(conn, tosort_root, Disposition::Consume).unwrap();
    upsert_file(conn, "AAAA", None, None, None, 7).unwrap();
    let ts = get_source_by_path(conn, tosort_root).unwrap().unwrap();
    let lib = get_source_by_path(conn, library_root).unwrap().unwrap();
    upsert_file_location(conn, "AAAA", ts.id, "game.zip", None).unwrap();
    upsert_file_location(conn, "AAAA", lib.id, "game.zip", None).unwrap();

    let sources = crate::db::files::list_sources(conn).unwrap();
    let tosort_abs = format!("{}/game.zip", tosort_root);

    // Library copy present on disk → safe to delete the staging copy.
    assert!(delete_has_surviving_copy(conn, &sources, &tosort_abs).unwrap());

    // Library copy gone on disk (stale catalogue record) → refuse the delete.
    std::fs::remove_file(library.path().join("game.zip")).unwrap();
    assert!(!delete_has_surviving_copy(conn, &sources, &tosort_abs).unwrap());
}

// A `preserve` source must not be emptied because a copy exists in another
// tree: only a same-tree copy authorises the delete. The default disposition
// is preserve, so this is the safe baseline.
#[test]
fn delete_of_preserve_file_refused_when_only_copy_is_in_another_tree() {
    let master = tempfile::TempDir::new().unwrap();
    let library = tempfile::TempDir::new().unwrap();
    let master_root = master.path().to_str().unwrap();
    let library_root = library.path().to_str().unwrap();

    // The same content sits in a preserve reference master and in the library.
    std::fs::write(master.path().join("game.zip"), b"content").unwrap();
    std::fs::write(library.path().join("game.zip"), b"content").unwrap();

    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    add_source(conn, master_root, false).unwrap(); // defaults to preserve
    add_source(conn, library_root, false).unwrap();
    upsert_file(conn, "AAAA", None, None, None, 7).unwrap();
    let master_src = get_source_by_path(conn, master_root).unwrap().unwrap();
    let lib = get_source_by_path(conn, library_root).unwrap().unwrap();
    upsert_file_location(conn, "AAAA", master_src.id, "game.zip", None).unwrap();
    upsert_file_location(conn, "AAAA", lib.id, "game.zip", None).unwrap();

    let sources = crate::db::files::list_sources(conn).unwrap();
    let master_abs = format!("{}/game.zip", master_root);

    // A library copy in a different tree does NOT authorise emptying the
    // preserve master — that would lose content the master's tree held.
    assert!(!delete_has_surviving_copy(conn, &sources, &master_abs).unwrap());
}

// Within a single preserve tree, a duplicate (the same content at a second
// path) may be dropped — the content survives in the same tree.
#[test]
fn delete_of_preserve_file_allowed_when_a_same_tree_copy_survives() {
    let master = tempfile::TempDir::new().unwrap();
    let master_root = master.path().to_str().unwrap();

    // The same content sits at two paths within the one preserve tree.
    std::fs::write(master.path().join("dup.zip"), b"content").unwrap();
    std::fs::write(master.path().join("canonical.zip"), b"content").unwrap();

    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    add_source(conn, master_root, false).unwrap(); // preserve
    upsert_file(conn, "AAAA", None, None, None, 7).unwrap();
    let src = get_source_by_path(conn, master_root).unwrap().unwrap();
    upsert_file_location(conn, "AAAA", src.id, "dup.zip", None).unwrap();
    upsert_file_location(conn, "AAAA", src.id, "canonical.zip", None).unwrap();

    let sources = crate::db::files::list_sources(conn).unwrap();
    let dup_abs = format!("{}/dup.zip", master_root);

    // The canonical copy in the same tree survives → dropping the duplicate
    // loses nothing. Once that same-tree copy is gone, the delete is refused.
    assert!(delete_has_surviving_copy(conn, &sources, &dup_abs).unwrap());
    std::fs::remove_file(master.path().join("canonical.zip")).unwrap();
    assert!(!delete_has_surviving_copy(conn, &sources, &dup_abs).unwrap());
}

// A path whose contents aren't catalogued can't be reasoned about — refuse.
#[test]
fn delete_refused_for_uncatalogued_path() {
    let tosort = tempfile::TempDir::new().unwrap();
    let root = tosort.path().to_str().unwrap();
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    add_source(conn, root, false).unwrap();
    let sources = crate::db::files::list_sources(conn).unwrap();
    let abs = format!("{}/unknown.zip", root);
    assert!(!delete_has_surviving_copy(conn, &sources, &abs).unwrap());
}

// The rollback-coherence guarantee for a drained staging container. A plan
// that drained a container left two coupled journal entries: the repack
// (reverse = delete the destination archive) and the drain (reverse = rebuild
// the container from that destination). Because rollback runs in reverse plan
// order and the drain is emitted last, the container is rebuilt from the
// destination *before* the destination is deleted — so rolling back restores
// the container's content rather than losing it. If the order were wrong, the
// rebuild would find the destination already gone and fail; this test passing
// (container restored, destination removed) is exactly that ordering holding.
#[test]
fn rolling_back_a_drained_container_rebuilds_it_before_the_destination_is_deleted() {
    use crate::plan::executor::execute_repack;
    use crate::plan::{OperationLog, RebuildEntry, SourceRef};
    use crate::util::hex_upper;
    use sha1::{Digest, Sha1};
    use std::io::Read;
    use std::path::Path;

    let sha1_of = |bytes: &[u8]| hex_upper(Sha1::digest(bytes));

    let tmp = tempfile::TempDir::new().unwrap();
    let data_dir = tmp.path().join("data");
    let logs_dir = data_dir.join("objects/logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Two entries the container held, now consolidated into the library
    // archive under their canonical names. The container named them
    // differently, to prove the rebuild restores the in-container names.
    let own = b"own-rom-data".as_slice();
    let common = b"common-cue-data".as_slice();
    let sha_own = sha1_of(own);
    let sha_common = sha1_of(common);

    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let loose_own = src_dir.join("own.rom");
    let loose_common = src_dir.join("common.rom");
    std::fs::write(&loose_own, own).unwrap();
    std::fs::write(&loose_common, common).unwrap();

    // The destination archive the repack built (the surviving content).
    let lib = tmp.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let dest = lib.join("GameOne.zip").to_str().unwrap().to_string();
    let dest_sources = vec![
        SourceRef {
            path: loose_own.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: sha_own.clone(),
            entry_name: Some("own.rom".to_string()),
        },
        SourceRef {
            path: loose_common.to_str().unwrap().to_string(),
            archive_path: None,
            sha1: sha_common.clone(),
            entry_name: Some("common.rom".to_string()),
        },
    ];
    execute_repack(&dest_sources, &dest, "zip", false).unwrap();

    // The container was drained (deleted) during the forward apply, so it does
    // not exist now — rollback must recreate it.
    let container = tmp
        .path()
        .join("tosort/g1.zip")
        .to_str()
        .unwrap()
        .to_string();
    assert!(!Path::new(&container).exists());

    // The journal as the forward apply wrote it: the repack first (reverse =
    // delete dest), the drain last (reverse = rebuild container from dest).
    let mut log = OperationLog::new("rollbackcoherence".to_string());
    log.log_repack(
        0,
        &[
            loose_own.to_str().unwrap().to_string(),
            loose_common.to_str().unwrap().to_string(),
        ],
        &dest,
        &[],
        true,
    );
    log.log_container_drain(
        1,
        &container,
        "zip",
        &[
            RebuildEntry {
                dest: dest.clone(),
                dest_entry: "own.rom".to_string(),
                container_entry: "track01.bin".to_string(),
                sha1: sha_own.clone(),
            },
            RebuildEntry {
                dest: dest.clone(),
                dest_entry: "common.rom".to_string(),
                container_entry: "track02.cue".to_string(),
                sha1: sha_common.clone(),
            },
        ],
        true,
    );
    log.complete();
    log.save(&logs_dir).unwrap();

    super::run_rollback(false, false, Some(data_dir)).unwrap();

    // The destination archive is gone (the repack's reverse ran)...
    assert!(
        !Path::new(&dest).exists(),
        "the destination archive should be removed by the repack's reverse"
    );
    // ...and the container is back, with its original in-container entry names
    // and byte-faithful content — which can only have happened if the rebuild
    // ran while the destination still existed.
    assert!(
        Path::new(&container).exists(),
        "the drained container should be rebuilt by rollback"
    );
    let file = std::fs::File::open(&container).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    assert_eq!(archive.len(), 2, "both entries restored");
    let mut read_entry = |name: &str| {
        let mut e = archive.by_name(name).unwrap();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).unwrap();
        buf
    };
    assert_eq!(read_entry("track01.bin"), own);
    assert_eq!(read_entry("track02.cue"), common);
}
