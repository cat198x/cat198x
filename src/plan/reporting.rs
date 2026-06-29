use super::rules::MAX_MATCH_ROWS;

pub(crate) fn shared_content(count: usize) {
    println!("{count} shared content(s) span multiple entries — copied to each, not moved.");
}

pub(crate) fn shared_containers(count: usize) {
    println!("{count} container(s) source multiple games — repacked per game, not relocated.");
}

pub(crate) fn oversized_collection(collection_name: &str) {
    println!(
        "Skipping {collection_name} — match expansion exceeds the {MAX_MATCH_ROWS}-row memory cap \
         (a meta-aggregate, not a placeable romset)."
    );
}

pub(crate) fn planning_collection(collection_name: &str, version: &str) {
    println!("Planning for: {collection_name} (v{version})");
}

pub(crate) fn merged_mode_not_implemented(collection_name: &str) {
    println!(
        "  note: merged mode is not yet implemented in the planner; \
         planning {collection_name} as non-merged."
    );
}

pub(crate) fn one_g_one_r(original_count: usize, filtered_count: usize) {
    println!(
        "  1G1R: {original_count} -> {filtered_count} ROMs (filtered {} variants)",
        original_count - filtered_count
    );
}

pub(crate) fn loose_summary(already_correct: usize, to_write: usize, deduped: usize) {
    println!(
        "  {already_correct} already correct, {to_write} to place, {deduped} duplicate(s) to delete"
    );
}

pub(crate) fn archive_summary(
    already_correct: usize,
    relocated: usize,
    to_write: usize,
    deduped: usize,
) {
    println!(
        "  {already_correct} ROMs already archived, {relocated} to relocate, \
         {to_write} archive(s) to build, {deduped} duplicate(s) to delete"
    );
}

pub(crate) fn disk_summary(already_correct: usize, to_write: usize, deduped: usize) {
    println!(
        "  {already_correct} CHD(s) already correct, {to_write} to place, \
         {deduped} duplicate(s) to delete"
    );
}

pub(crate) fn skipped_no_dest(count: usize) {
    println!();
    println!("{count} collection(s) skipped — no destination resolved.");
    println!("  Set one per collection:  cat198x config set <collection> dest_path <path>");
    println!("  or library-wide:         cat198x config set-default dest_path <path>");
}

pub(crate) fn skipped_oversized_rollup(count: usize) {
    println!();
    println!(
        "{count} collection(s) skipped — match expansion over the {MAX_MATCH_ROWS}-row memory cap."
    );
}

pub(crate) fn no_matching_filter(pattern: &str) {
    println!("No collections match the filter: {pattern}");
}

pub(crate) fn no_active_collections() {
    println!("No collections with an active version to plan.");
}
