use crate::config::{MergeMode, OutputFormat};

/// Options controlling plan generation.
#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// Glob over collection names; `None` plans every collection.
    pub dat_filter: Option<String>,
    /// Restrict planning to these sets — the top segment of a collection's
    /// library path (e.g. `TOSEC`, `TOSEC-PIX`, `FinalBurn Neo`). `None` plans
    /// every set; useful to scope one set's work (e.g. ingest TOSEC without the
    /// arcade sets) without listing every collection.
    pub set_filter: Option<Vec<String>>,
    /// Library-wide destination root for collections without their own dest_path.
    pub default_dest: Option<String>,
    /// Output format for collections without their own setting.
    pub default_format: OutputFormat,
    /// Merge mode for collections without their own setting. Controls MAME-style
    /// parent/clone placement: `Split` (the implemented target) drops a clone's
    /// merge-tagged inherited ROMs from its placement — they live in the parent —
    /// so the clone's archive/folder holds only its own unique ROMs. `NonMerged`
    /// (the default) places every ROM a game's DAT entry lists, parent or clone.
    pub default_merge_mode: MergeMode,
}
