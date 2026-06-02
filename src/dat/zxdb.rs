//! Generate a Logiqx DAT from the ZXDB MySQL dump.
//!
//! [ZXDB](https://github.com/zxdb/ZXDB) is the canonical open database for
//! Sinclair machines. Its `downloads` table records `file_link`, `file_size`
//! and `file_md5` for every preserved file — covering the World of Spectrum /
//! Spectrum Computing content that TOSEC's thin Sinclair sets miss. But ZXDB
//! ships as a MySQL dump, not a DAT, so this module parses the `downloads`
//! INSERT statements and emits a clrmamepro/Logiqx DAT keyed on MD5. Combined
//! with MD5 match support in the catalogue, that gives Cat198x a Spectrum
//! verification authority for the WoS-only material.
//!
//! The dump is parsed by streaming lines and tokenising the `(...)` tuples with
//! quote/escape awareness — no SQL engine, no loading 140 MB into memory.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

/// Column positions in the ZXDB `downloads` table (0-based), as emitted by the
/// MySQL dump's `INSERT INTO \`downloads\` (...)` statements.
const COL_FILE_LINK: usize = 3;
const COL_FILE_SIZE: usize = 5;
const COL_FILE_MD5: usize = 6;

/// One downloadable file from ZXDB that carries an MD5 we can verify against.
#[derive(Debug, PartialEq, Eq)]
struct ZxdbRom {
    /// Unique game name — the file_link path (minus leading slash). Unique per
    /// download, so generated `<game>` names never collide.
    game: String,
    /// ROM name — the file's basename, for display and reorganisation.
    name: String,
    size: u64,
    /// Uppercase 32-hex MD5.
    md5: String,
}

/// Parse the ZXDB dump at `sql_path` and write a Logiqx DAT to `out_path`.
/// Returns the number of ROM entries written (rows with a usable MD5).
pub fn generate_dat(sql_path: &Path, out_path: &Path) -> Result<usize> {
    let reader = BufReader::new(
        File::open(sql_path)
            .with_context(|| format!("Failed to open ZXDB dump: {:?}", sql_path))?,
    );
    let mut writer = BufWriter::new(
        File::create(out_path).with_context(|| format!("Failed to create DAT: {:?}", out_path))?,
    );

    write_dat_header(&mut writer)?;

    let mut in_downloads = false;
    let mut count = 0usize;

    for line in reader.lines() {
        let line = line.context("Failed to read line from ZXDB dump")?;
        let trimmed = line.trim_start();

        // A new downloads INSERT batch. Tuples may follow on this same line
        // (after `VALUES`) or on subsequent lines until the statement ends.
        if trimmed.starts_with("INSERT INTO `downloads`") {
            in_downloads = true;
            if let Some(idx) = line.find(" VALUES") {
                count += emit_tuples(&line[idx + " VALUES".len()..], &mut writer)?;
            }
            if line.trim_end().ends_with(';') {
                in_downloads = false;
            }
            continue;
        }

        if in_downloads {
            count += emit_tuples(&line, &mut writer)?;
            if line.trim_end().ends_with(';') {
                in_downloads = false;
            }
        }
    }

    write_dat_footer(&mut writer)?;
    writer.flush().context("Failed to flush generated DAT")?;
    Ok(count)
}

/// Parse every `(...)` tuple in a line of VALUES data and write a `<game>` for
/// each that has a usable MD5. Returns how many were written.
fn emit_tuples(line: &str, writer: &mut impl Write) -> Result<usize> {
    let mut count = 0;
    for body in extract_tuples(line) {
        if let Some(rom) = tuple_to_rom(&body) {
            write_game(writer, &rom)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Extract the inner text of each top-level `(...)` group in `line`, respecting
/// single-quoted strings and backslash escapes (so commas and parens inside
/// quoted fields are not treated as structure).
fn extract_tuples(line: &str) -> Vec<String> {
    let mut tuples = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_str = false;
    let mut escaped = false;

    for (i, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            match ch {
                '\\' => escaped = true,
                '\'' => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '\'' => in_str = true,
            '(' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && start <= i {
                    tuples.push(line[start..i].to_string());
                }
            }
            _ => {}
        }
    }
    tuples
}

/// Turn a tuple body into a ROM, or `None` if it has no usable MD5.
fn tuple_to_rom(body: &str) -> Option<ZxdbRom> {
    let fields = split_fields(body);
    if fields.len() <= COL_FILE_MD5 {
        return None;
    }
    let md5 = unquote(&fields[COL_FILE_MD5])?;
    if md5.len() != 32 || !md5.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let link = unquote(&fields[COL_FILE_LINK])?;
    let name = link.rsplit('/').next().unwrap_or(&link).to_string();
    if name.is_empty() {
        return None;
    }
    let size = fields[COL_FILE_SIZE].trim().parse::<u64>().unwrap_or(0);
    Some(ZxdbRom {
        game: link.trim_start_matches('/').to_string(),
        name,
        size,
        md5: md5.to_ascii_uppercase(),
    })
}

/// Split a tuple body into raw comma-separated field tokens, respecting
/// single-quoted strings and escapes. Quotes are kept; [`unquote`] strips them.
fn split_fields(body: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut escaped = false;

    for ch in body.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
        } else if in_str {
            match ch {
                '\\' => {
                    cur.push(ch);
                    escaped = true;
                }
                '\'' => {
                    cur.push(ch);
                    in_str = false;
                }
                _ => cur.push(ch),
            }
        } else {
            match ch {
                '\'' => {
                    cur.push(ch);
                    in_str = true;
                }
                ',' => {
                    fields.push(cur.trim().to_string());
                    cur.clear();
                }
                _ => cur.push(ch),
            }
        }
    }
    fields.push(cur.trim().to_string());
    fields
}

/// Strip the surrounding quotes from a MySQL string token and unescape it.
/// Returns `None` for the literal `NULL`.
fn unquote(token: &str) -> Option<String> {
    let t = token.trim();
    if t == "NULL" {
        return None;
    }
    let inner = t.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                // \' \" \\ and anything else → the literal character
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn write_dat_header(writer: &mut impl Write) -> Result<()> {
    writeln!(writer, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(writer, "<datafile>")?;
    writeln!(writer, "  <header>")?;
    writeln!(writer, "    <name>ZXDB</name>")?;
    writeln!(
        writer,
        "    <description>ZXDB (Sinclair) — MD5 catalogue generated by cat198x from the ZXDB downloads table</description>"
    )?;
    writeln!(
        writer,
        "    <homepage>https://github.com/zxdb/ZXDB</homepage>"
    )?;
    writeln!(writer, "  </header>")?;
    Ok(())
}

fn write_game(writer: &mut impl Write, rom: &ZxdbRom) -> Result<()> {
    let game = xml_escape(&rom.game);
    let name = xml_escape(&rom.name);
    writeln!(writer, r#"  <game name="{game}">"#)?;
    writeln!(
        writer,
        r#"    <rom name="{name}" size="{}" md5="{}"/>"#,
        rom.size, rom.md5
    )?;
    writeln!(writer, "  </game>")?;
    Ok(())
}

fn write_dat_footer(writer: &mut impl Write) -> Result<()> {
    writeln!(writer, "</datafile>")?;
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_tuple() {
        let line = "\t(153, 1000967, 0, '/zxdb/sinclair/entries/1000967/VentamaticMidiInterface.jpg', NULL, 14245, '85a60f488607ffb0dbac35ece7f3e79c', 48, 0, NULL),";
        let tuples = extract_tuples(line);
        assert_eq!(tuples.len(), 1);
        let rom = tuple_to_rom(&tuples[0]).expect("has md5");
        assert_eq!(rom.name, "VentamaticMidiInterface.jpg");
        assert_eq!(
            rom.game,
            "zxdb/sinclair/entries/1000967/VentamaticMidiInterface.jpg"
        );
        assert_eq!(rom.size, 14245);
        assert_eq!(rom.md5, "85A60F488607FFB0DBAC35ECE7F3E79C");
    }

    #[test]
    fn skips_null_md5() {
        let line = "(1, 2, 0, '/x/y.tap', NULL, 100, NULL, 1, 0, NULL),";
        let tuples = extract_tuples(line);
        assert_eq!(tuples.len(), 1);
        assert!(tuple_to_rom(&tuples[0]).is_none());
    }

    #[test]
    fn handles_escaped_apostrophe_in_filename() {
        // SQL field: '/x/O\'Brien.tap' — the escaped quote must not end the string.
        let line =
            "(1, 2, 0, '/x/O\\'Brien.tap', NULL, 100, 'abcdef01abcdef01abcdef01abcdef01', 1),";
        let tuples = extract_tuples(line);
        assert_eq!(tuples.len(), 1, "escaped quote kept the tuple intact");
        let rom = tuple_to_rom(&tuples[0]).expect("has md5");
        assert_eq!(rom.name, "O'Brien.tap");
        assert_eq!(rom.md5, "ABCDEF01ABCDEF01ABCDEF01ABCDEF01");
    }

    #[test]
    fn parses_multiple_tuples_on_one_line() {
        let line = "(1,2,0,'/a/one.tap',NULL,10,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1),(2,3,0,'/b/two.z80',NULL,20,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',1);";
        let roms: Vec<_> = extract_tuples(line)
            .iter()
            .filter_map(|t| tuple_to_rom(t))
            .collect();
        assert_eq!(roms.len(), 2);
        assert_eq!(roms[0].name, "one.tap");
        assert_eq!(roms[1].name, "two.z80");
    }

    #[test]
    fn rejects_non_hex_md5() {
        // A 32-char but non-hex value must be rejected, not emitted.
        let line = "(1,2,0,'/a/x.tap',NULL,10,'not-a-real-md5-zzzzzzzzzzzzzzzzzz',1),";
        let tuples = extract_tuples(line);
        assert!(tuple_to_rom(&tuples[0]).is_none());
    }

    #[test]
    fn emits_parseable_logiqx() {
        let rom = ZxdbRom {
            game: "zxdb/sinclair/entries/1/Crash & Burn.tap".to_string(),
            name: "Crash & Burn.tap".to_string(),
            size: 1234,
            md5: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        };
        let mut out = Vec::new();
        write_dat_header(&mut out).unwrap();
        write_game(&mut out, &rom).unwrap();
        write_dat_footer(&mut out).unwrap();
        let xml = String::from_utf8(out).unwrap();

        // The ampersand must be escaped, and the catalogue's own parser must
        // read it back with the md5 intact.
        assert!(xml.contains("Crash &amp; Burn.tap"));
        let (header, games) = crate::dat::parser::parse_dat(&xml).expect("parses");
        assert_eq!(header.name, "ZXDB");
        assert_eq!(games.len(), 1);
        assert_eq!(
            games[0].roms[0].md5.as_deref(),
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        );
    }
}
