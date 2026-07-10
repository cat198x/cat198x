//! File and location CRUD operations.
//!
//! This module preserves the public `crate::db::files` surface while keeping
//! source, content, and physical-location database operations in smaller files.

mod content;
mod locations;
mod sources;
mod types;

pub use content::{
    get_file_by_sha1, has_matching_crc_size, has_matching_file, has_matching_md5, upsert_file,
};
pub use locations::{
    catalogued_paths, contents_at_location, count_files_in_source, get_file_locations,
    relocate_locations, remove_locations_at, remove_stale_locations, upsert_file_location,
};
pub use sources::{
    add_source, get_source_by_path, list_sources, remove_source, resolve_in_sources,
    set_source_disposition, update_source_scanned,
};
pub use types::{Disposition, File, FileLocation, Source};

#[cfg(test)]
mod tests;
