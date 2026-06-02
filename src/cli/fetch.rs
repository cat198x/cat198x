//! DAT fetch command - download DAT files from known sources

use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

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
    DatSource {
        name: "zxdb",
        description: "ZXDB (Sinclair) — MD5 DAT generated from the canonical database; covers WoS + Spectrum Computing where TOSEC is thin",
        url_pattern: "https://github.com/zxdb/ZXDB/raw/master/ZXDB_mysql.sql.zip",
        source_type: "zxdb",
    },
    DatSource {
        name: "tosec",
        description: "TOSEC complete DAT pack (guided — no stable auto-URL; download then `dat add -r`)",
        url_pattern: "https://www.tosecdev.org/downloads",
        source_type: "manual",
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

        // Some sources have no stable auto-download URL (TOSEC's dated portal,
        // No-Intro's auth wall) — guide the user instead of fetching a rot-prone
        // link.
        if source.source_type == "manual" {
            return print_manual_source_help(source);
        }

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

        // ZXDB ships as a MySQL dump, not a DAT — generate a Logiqx DAT from it.
        let import_path = if source.source_type == "zxdb" {
            generate_zxdb_dat(&output_path)?
        } else {
            output_path
        };

        println!();
        println!("To import this DAT, run:");
        println!("  cat198x dat add {:?}", import_path);

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

    Ok(())
}

/// Generate a Logiqx DAT from a downloaded ZXDB MySQL dump zip.
///
/// Unzips the `.sql` dump beside the download, parses its `downloads` table,
/// and writes `zxdb.dat` alongside. Returns the path to the generated DAT.
fn generate_zxdb_dat(zip_path: &Path) -> Result<PathBuf> {
    println!();
    println!("Extracting ZXDB dump...");
    let sql_path = unzip_first_sql(zip_path)?;

    let dat_path = zip_path.with_file_name("zxdb.dat");
    println!("Generating MD5 DAT from the ZXDB downloads table...");
    let count = crate::dat::zxdb::generate_dat(&sql_path, &dat_path)?;
    println!("Wrote {} verifiable entries to {:?}", count, dat_path);

    Ok(dat_path)
}

/// Extract the first `.sql` entry from `zip_path` to a file beside it.
fn unzip_first_sql(zip_path: &Path) -> Result<PathBuf> {
    let file =
        File::open(zip_path).with_context(|| format!("Failed to open ZXDB zip: {:?}", zip_path))?;
    let mut archive = zip::ZipArchive::new(file).context("Failed to read ZXDB zip")?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.name().to_ascii_lowercase().ends_with(".sql") {
            // Basename only, to avoid zip-slip into a parent directory.
            let name = Path::new(entry.name())
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "zxdb.sql".to_string());
            let out_path = zip_path.with_file_name(name);
            let mut out_file = File::create(&out_path)
                .with_context(|| format!("Failed to create {:?}", out_path))?;
            std::io::copy(&mut entry, &mut out_file).context("Failed to extract .sql")?;
            return Ok(out_path);
        }
    }
    anyhow::bail!("No .sql file found inside the ZXDB zip")
}

/// Print guidance for a source that can't be auto-downloaded (no stable URL or
/// an auth wall), rather than fetching a link that will rot.
fn print_manual_source_help(source: &DatSource) -> Result<()> {
    println!(
        "'{}' has no stable auto-download URL — fetch it manually:",
        source.name
    );
    println!();
    println!("  1. Open {}", source.url_pattern);
    println!("  2. Download the latest complete DAT pack and unzip it");
    println!("  3. Import the whole tree at once:");
    println!("       cat198x dat add -r <unzipped-dat-pack-dir>");
    println!();
    println!("If you already have a direct URL to a single DAT file:");
    println!("       cat198x dat fetch --url <URL>");
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
