use crate::db::files;

/// Whether a `--source` selector picks this source: a purely numeric selector
/// is a source id and matches exactly; anything else matches as a path
/// substring.
pub(crate) fn source_matches(source: &files::Source, selector: &str) -> bool {
    match selector.parse::<i64>() {
        Ok(id) => source.id == id,
        Err(_) => source.path.contains(selector),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: i64, path: &str) -> files::Source {
        files::Source {
            id,
            path: path.to_string(),
            case_sensitive: false,
            added_at: String::new(),
            last_scanned: None,
            disposition: files::Disposition::Preserve,
        }
    }

    #[test]
    fn numeric_selector_matches_id_only() {
        let selected = source(42, "/roms/42");
        let contains_digits = source(7, "/roms/set-42");

        assert!(source_matches(&selected, "42"));
        assert!(!source_matches(&contains_digits, "42"));
    }

    #[test]
    fn non_numeric_selector_matches_path_substring() {
        let selected = source(42, "/roms/ToSort/NES");

        assert!(source_matches(&selected, "ToSort"));
        assert!(!source_matches(&selected, "Master"));
    }
}
