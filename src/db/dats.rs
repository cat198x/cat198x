//! DAT node, game, ROM, and completeness operations

mod queries;
mod stats;
mod types;
mod writes;

pub use queries::{
    count_games_and_roms, find_rom_by_sha1, get_game_by_name, get_games_for_node,
    get_games_for_version, get_roms_for_game, get_roms_for_version, get_roms_for_version_grouped,
};
pub use stats::{
    GameRomRequirements, MergeModeStats, RequirementOptions, RomKey, calculate_merge_mode_stats,
    calculate_rom_requirements, calculate_rom_requirements_with_options, present_keys, rom_present,
};
pub use types::{DatGame, DatNode, DatRom, MergeMode};
pub use writes::{
    create_disk, create_game, create_node, create_rom, nest_primary_node_under_name,
    primary_node_path, rename_dat_node,
};

#[cfg(test)]
mod tests;
