//! DAT fetch command - download DAT files from known sources

use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

/// Known DAT sources
#[derive(Debug, Clone)]
pub struct DatSource {
    pub name: &'static str,
    pub description: &'static str,
    pub url_pattern: &'static str,
    pub source_type: &'static str,
}

/// Built-in DAT sources
pub const KNOWN_SOURCES: &[DatSource] = &[
    DatSource {
        name: "mame",
        description: "MAME arcade DAT (latest stable)",
        url_pattern: "https://github.com/mamedev/mame/releases/latest/download/mame.zip",
        source_type: "mame",
    },
    DatSource {
        name: "mame-softlist",
        description: "MAME software lists (hash directory)",
        url_pattern: "https://github.com/mamedev/mame/archive/refs/heads/master.zip",
        source_type: "mame",
    },
    DatSource {
        name: "libretro-dats",
        description: "Libretro DAT collection (No-Intro mirror)",
        url_pattern: "https://github.com/libretro/libretro-database/archive/refs/heads/master.zip",
        source_type: "nointro",
    },
    DatSource {
        name: "fbneo",
        description: "FinalBurn Neo DAT",
        url_pattern: "https://github.com/libretro/FBNeo/raw/master/dats/FinalBurn%20Neo%20(ClrMame%20Pro%20XML%2C%20Arcade%20only).dat",
        source_type: "mame",
    },
];

/// Run the fetch command
pub fn run(
    source: Option<&str>,
    url: Option<&str>,
    output: Option<PathBuf>,
    list: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    if list {
        return list_sources();
    }

    // If URL provided, download directly
    if let Some(url) = url {
        let output_path = output.unwrap_or_else(|| {
            // Extract filename from URL or use default
            url.rsplit('/')
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("downloaded.dat"))
        });
        return download_dat(url, &output_path);
    }

    // If source name provided, look it up
    if let Some(source_name) = source {
        let source = KNOWN_SOURCES
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(source_name))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown source '{}'. Use --list to see available sources.",
                    source_name
                )
            })?;

        // Determine output path
        let output_path = output.unwrap_or_else(|| {
            let data_dir = data_dir
                .or_else(|| {
                    directories::ProjectDirs::from("", "", "cat198x")
                        .map(|d| d.data_dir().to_path_buf())
                })
                .unwrap_or_else(|| PathBuf::from("."));

            let downloads_dir = data_dir.join("downloads");
            fs::create_dir_all(&downloads_dir).ok();

            let filename = source
                .url_pattern
                .rsplit('/')
                .next()
                .unwrap_or("download.dat");

            downloads_dir.join(filename)
        });

        println!("Fetching {} DAT...", source.name);
        println!("Source: {}", source.description);

        download_dat(source.url_pattern, &output_path)?;

        println!();
        println!("To import this DAT, run:");
        println!("  cat198x dat add {:?}", output_path);

        return Ok(());
    }

    // No arguments - show help
    println!("Usage: cat198x dat fetch <SOURCE> [--output <PATH>]");
    println!("       cat198x dat fetch --url <URL> [--output <PATH>]");
    println!("       cat198x dat fetch --list");
    println!();
    println!("Use --list to see available DAT sources.");

    Ok(())
}

/// List available DAT sources
fn list_sources() -> Result<()> {
    println!("Available DAT sources:");
    println!();

    for source in KNOWN_SOURCES {
        println!("  {}", source.name);
        println!("    {}", source.description);
        println!("    Type: {}", source.source_type);
        println!();
    }

    println!("Usage:");
    println!("  cat198x dat fetch mame              # Download MAME DAT");
    println!("  cat198x dat fetch --url <URL>       # Download from custom URL");
    println!();
    println!("Note: Some sources (like No-Intro) require manual download due to");
    println!("authentication. Visit https://datomatic.no-intro.org for official DATs.");

    Ok(())
}

/// Download a DAT file from URL
fn download_dat(url: &str, output_path: &PathBuf) -> Result<()> {
    use std::time::Duration;

    println!("Downloading from: {}", url);
    println!("Saving to: {:?}", output_path);
    println!();

    // Create parent directories if needed
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Use reqwest for HTTP download (blocking)
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(format!("cat198x/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(url)
        .send()
        .context("Failed to connect to server")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Download failed: HTTP {} {}",
            response.status().as_u16(),
            response.status().canonical_reason().unwrap_or("Unknown")
        );
    }

    // Get content length for progress
    let content_length = response.content_length();

    // Download content
    let bytes = response.bytes().context("Failed to download file")?;

    // Write to file
    let file = File::create(output_path)
        .with_context(|| format!("Failed to create file: {:?}", output_path))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(&bytes)?;
    writer.flush()?;

    let size_str = if let Some(len) = content_length {
        crate::util::format_bytes(len)
    } else {
        crate::util::format_bytes(bytes.len() as u64)
    };

    println!("Downloaded {} successfully", size_str);

    // If it's a zip, offer to extract
    if output_path.extension().is_some_and(|ext| ext == "zip") {
        println!();
        println!("Note: Downloaded file is a ZIP archive.");
        println!("Extract it manually or use: unzip {:?}", output_path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_sources_not_empty() {
        assert!(!KNOWN_SOURCES.is_empty());
    }

    #[test]
    fn test_source_lookup() {
        let mame = KNOWN_SOURCES.iter().find(|s| s.name == "mame");
        assert!(mame.is_some());
        assert!(mame.unwrap().url_pattern.contains("mame"));
    }

    #[test]
    fn test_case_insensitive_lookup() {
        let mame_lower = KNOWN_SOURCES
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case("MAME"));
        assert!(mame_lower.is_some());
    }
}
