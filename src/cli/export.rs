//! Export command - export collection status in various formats

use anyhow::Result;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use crate::db::{collections, dats, files};

use super::open_database;

/// Export formats
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExportFormat {
    Text,
    Csv,
    Json,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "txt" | "text" => Some(Self::Text),
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn extension(&self) -> &str {
        match self {
            Self::Text => "txt",
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }
}

/// Run export command
pub fn run(
    collection: &str,
    output: Option<PathBuf>,
    format: Option<&str>,
    have_only: bool,
    missing_only: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Find the collection
    let coll = collections::get_collection_by_name(conn, collection)?
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", collection))?;

    // Get active version
    let version = collections::get_active_version(conn, coll.id)?
        .ok_or_else(|| anyhow::anyhow!("No active version for '{}'", collection))?;

    // Determine format
    let export_format = if let Some(fmt) = format {
        ExportFormat::parse(fmt)
            .ok_or_else(|| anyhow::anyhow!("Unknown format: {}. Use txt, csv, or json", fmt))?
    } else if let Some(ref path) = output {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(ExportFormat::parse)
            .unwrap_or(ExportFormat::Text)
    } else {
        ExportFormat::Text
    };

    // Get all games and ROMs for this version
    let games = dats::get_games_for_version(conn, version.id)?;
    let roms = dats::get_roms_for_version(conn, version.id)?;

    // Build ROM status list
    #[derive(serde::Serialize)]
    struct RomStatus {
        game: String,
        rom: String,
        sha1: String,
        have: bool,
        source_path: Option<String>,
    }

    let mut status_list = Vec::new();

    for (game_name, rom) in &roms {
        let sha1 = rom.sha1.clone().unwrap_or_default();
        let locations = if sha1.is_empty() {
            Vec::new()
        } else {
            files::get_file_locations(conn, &sha1)?
        };
        let have = !locations.is_empty();

        // Get source path if we have it
        let source_path = locations.first().map(|loc| loc.path.clone());

        status_list.push(RomStatus {
            game: game_name.clone(),
            rom: rom.name.clone(),
            sha1,
            have,
            source_path,
        });
    }

    // Filter based on flags
    let filtered: Vec<_> = status_list
        .iter()
        .filter(|s| {
            if have_only {
                s.have
            } else if missing_only {
                !s.have
            } else {
                true
            }
        })
        .collect();

    // Calculate stats
    let total_roms = status_list.len();
    let have_count = status_list.iter().filter(|s| s.have).count();
    let missing_count = total_roms - have_count;
    let completion = if total_roms > 0 {
        (have_count as f64 / total_roms as f64) * 100.0
    } else {
        0.0
    };

    // Output
    let output_to_file = output.is_some();
    let mut writer: Box<dyn Write> = if let Some(path) = &output {
        Box::new(BufWriter::new(File::create(path)?))
    } else {
        Box::new(std::io::stdout())
    };

    match export_format {
        ExportFormat::Text => {
            writeln!(writer, "Collection: {}", coll.name)?;
            writeln!(writer, "Version: {}", version.version)?;
            writeln!(writer, "Games: {}", games.len())?;
            writeln!(writer, "ROMs: {} total, {} have, {} missing ({:.1}%)",
                total_roms, have_count, missing_count, completion)?;
            writeln!(writer)?;

            if have_only {
                writeln!(writer, "Have list ({} ROMs):", filtered.len())?;
            } else if missing_only {
                writeln!(writer, "Missing list ({} ROMs):", filtered.len())?;
            } else {
                writeln!(writer, "ROM list ({} ROMs):", filtered.len())?;
            }
            writeln!(writer)?;

            for s in &filtered {
                let status = if s.have { "[HAVE]" } else { "[MISS]" };
                writeln!(writer, "{} {} - {}", status, s.game, s.rom)?;
                if !s.sha1.is_empty() {
                    writeln!(writer, "       SHA1: {}", s.sha1)?;
                }
                if let Some(ref path) = s.source_path {
                    writeln!(writer, "       Source: {}", path)?;
                }
            }
        }

        ExportFormat::Csv => {
            writeln!(writer, "game,rom,sha1,have,source_path")?;
            for s in &filtered {
                writeln!(writer, "\"{}\",\"{}\",{},{},\"{}\"",
                    s.game.replace('"', "\"\""),
                    s.rom.replace('"', "\"\""),
                    s.sha1,
                    s.have,
                    s.source_path.as_deref().unwrap_or("").replace('"', "\"\"")
                )?;
            }
        }

        ExportFormat::Json => {
            #[derive(serde::Serialize)]
            struct Export<'a> {
                collection: &'a str,
                version: &'a str,
                games: usize,
                total_roms: usize,
                have_count: usize,
                missing_count: usize,
                completion_percent: f64,
                roms: &'a [&'a RomStatus],
            }

            let export = Export {
                collection: &coll.name,
                version: &version.version,
                games: games.len(),
                total_roms,
                have_count,
                missing_count,
                completion_percent: completion,
                roms: &filtered,
            };

            let json = serde_json::to_string_pretty(&export)?;
            writeln!(writer, "{}", json)?;
        }
    }

    writer.flush()?;

    if output_to_file {
        println!("Exported {} ROMs to {:?}", filtered.len(), output.unwrap());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_from_str() {
        assert_eq!(ExportFormat::parse("txt"), Some(ExportFormat::Text));
        assert_eq!(ExportFormat::parse("text"), Some(ExportFormat::Text));
        assert_eq!(ExportFormat::parse("csv"), Some(ExportFormat::Csv));
        assert_eq!(ExportFormat::parse("json"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::parse("JSON"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::parse("unknown"), None);
    }

    #[test]
    fn test_format_extension() {
        assert_eq!(ExportFormat::Text.extension(), "txt");
        assert_eq!(ExportFormat::Csv.extension(), "csv");
        assert_eq!(ExportFormat::Json.extension(), "json");
    }
}
