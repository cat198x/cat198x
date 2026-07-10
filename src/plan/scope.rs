use super::options::PlanOptions;
use super::rules::glob_match;

pub(crate) fn collection_name_matches(name: &str, opts: &PlanOptions) -> bool {
    let Some(pattern) = opts.dat_filter.as_deref() else {
        return true;
    };

    glob_match(pattern, name)
}

pub(crate) fn hierarchy_matches_set_filter(hierarchy: &str, opts: &PlanOptions) -> bool {
    let Some(sets) = opts.set_filter.as_ref() else {
        return true;
    };

    let set = hierarchy.split('/').next().unwrap_or(hierarchy);
    sets.iter().any(|s| s == set)
}
