//! Logiqx XML DAT parser
//!
//! Uses quick-xml for streaming parsing, which is essential for large
//! DAT files like MAME (50MB+).

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::path::Path;

use super::types::*;

/// Parse a DAT file and return header + games
pub fn parse_dat_file(path: &Path) -> Result<(DatHeader, Vec<DatGameEntry>)> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read DAT file: {:?}", path))?;

    parse_dat(&contents)
}

/// Parse DAT content from a string
pub fn parse_dat(xml: &str) -> Result<(DatHeader, Vec<DatGameEntry>)> {
    let mut reader = Reader::from_str(xml);
    // Note: text is NOT trimmed per-event. quick-xml splits a run of character
    // data at every entity reference (see `read_element_text`), so trimming
    // each fragment would eat the spaces around an entity — e.g. turning
    // "Famicom &amp; Entertainment" into "Famicom&Entertainment". We accumulate
    // the raw fragments and trim only the final element text instead.

    let mut header = DatHeader::default();
    let mut games = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"header" => {
                    header = parse_header(&mut reader)?;
                }
                b"game" | b"machine" => {
                    let game = parse_game(&mut reader, &e)?;
                    games.push(game);
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Error parsing XML at position {}: {:?}",
                    reader.buffer_position(),
                    e
                ));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok((header, games))
}

/// Parse the <header> element
fn parse_header(reader: &mut Reader<&[u8]>) -> Result<DatHeader> {
    let mut header = DatHeader::default();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                // Read the child element's full text content in one go, so a
                // value split across entity references (e.g. "Shoot&apos;em
                // Up") is reassembled rather than truncated to its last chunk.
                let tag = e.name().as_ref().to_vec();
                let text = read_element_text(reader, &tag)?;
                match tag.as_slice() {
                    b"name" => header.name = text,
                    b"description" => header.description = Some(text),
                    b"version" => header.version = Some(text),
                    b"author" => header.author = Some(text),
                    b"homepage" => header.homepage = Some(text),
                    b"url" => header.url = Some(text),
                    b"category" => header.category = Some(text),
                    _ => {}
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"header" => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
        buf.clear();
    }

    Ok(header)
}

/// Parse a <game> or <machine> element
fn parse_game(
    reader: &mut Reader<&[u8]>,
    start: &quick_xml::events::BytesStart,
) -> Result<DatGameEntry> {
    let mut game = DatGameEntry {
        name: String::new(),
        description: None,
        clone_of: None,
        rom_of: None,
        is_bios: false,
        is_device: false,
        is_mechanical: false,
        roms: Vec::new(),
        devices: Vec::new(),
    };

    // Parse attributes
    for attr in start.attributes().flatten() {
        match attr.key.as_ref() {
            b"name" => game.name = attr_value(&attr)?.to_string(),
            b"cloneof" => game.clone_of = Some(attr_value(&attr)?.to_string()),
            b"romof" => game.rom_of = Some(attr_value(&attr)?.to_string()),
            b"isbios" => game.is_bios = attr.value.as_ref() == b"yes",
            b"isdevice" => game.is_device = attr.value.as_ref() == b"yes",
            b"ismechanical" => game.is_mechanical = attr.value.as_ref() == b"yes",
            _ => {}
        }
    }

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                // `<rom>…</rom>` and `<device_ref>…</device_ref>` carry all the
                // data we need on the start tag, so handle them like their
                // self-closing form — otherwise non-self-closing elements are
                // silently dropped (a game then looks ROM-less, i.e. "complete").
                // Any other element (e.g. <description>) has its full text read
                // and consumed up to its end tag.
                let consumed = handle_game_child(&e, &mut game)?;
                if !consumed {
                    let tag = e.name().as_ref().to_vec();
                    let text = read_element_text(reader, &tag)?;
                    if tag == b"description" {
                        game.description = Some(text);
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                handle_game_child(&e, &mut game)?;
            }
            Ok(Event::End(e)) if matches!(e.name().as_ref(), b"game" | b"machine") => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
        buf.clear();
    }

    Ok(game)
}

/// Handle a `<game>`/`<machine>` child that carries all its data on the start
/// tag — `<rom>` and `<device_ref>`. Works for both the self-closing
/// (`Event::Empty`) and non-self-closing (`Event::Start`) forms. Returns true
/// if the element was a recognised child and has been consumed.
fn handle_game_child(e: &quick_xml::events::BytesStart, game: &mut DatGameEntry) -> Result<bool> {
    match e.name().as_ref() {
        b"rom" => {
            game.roms.push(parse_rom_attrs(e)?);
            Ok(true)
        }
        b"disk" => {
            game.roms.push(parse_disk_attrs(e)?);
            Ok(true)
        }
        b"device_ref" => {
            for attr in e.attributes().flatten() {
                if attr.key.as_ref() == b"name" {
                    game.devices.push(attr_value(&attr)?.to_string());
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Parse ROM attributes from a `<rom>` element (self-closing or not)
fn parse_rom_attrs(e: &quick_xml::events::BytesStart) -> Result<DatRomEntry> {
    let mut rom = DatRomEntry {
        name: String::new(),
        size: 0,
        crc32: None,
        md5: None,
        sha1: None,
        status: RomStatus::Good,
        merge: None,
        is_disk: false,
    };

    for attr in e.attributes().flatten() {
        match attr.key.as_ref() {
            b"name" => rom.name = attr_value(&attr)?.to_string(),
            b"size" => rom.size = attr_value(&attr)?.parse().unwrap_or(0),
            b"crc" => rom.crc32 = Some(attr_value(&attr)?.to_uppercase()),
            b"md5" => rom.md5 = Some(attr_value(&attr)?.to_uppercase()),
            b"sha1" => rom.sha1 = Some(attr_value(&attr)?.to_uppercase()),
            b"status" => rom.status = RomStatus::parse(&attr_value(&attr)?),
            b"merge" => rom.merge = Some(attr_value(&attr)?.to_string()),
            _ => {}
        }
    }

    Ok(rom)
}

/// Parse a `<disk>` element (a CHD image) into a disk-flagged `DatRomEntry`.
///
/// Disks carry `name` and `sha1` (the CHD's internal logical-data hash), plus an
/// optional `md5` and `status` on older DATs. They have no `size` or `crc`. The
/// matched file on disk is `<name>.chd`, stored loose in a machine folder.
fn parse_disk_attrs(e: &quick_xml::events::BytesStart) -> Result<DatRomEntry> {
    let mut disk = DatRomEntry {
        name: String::new(),
        size: 0,
        crc32: None,
        md5: None,
        sha1: None,
        status: RomStatus::Good,
        merge: None,
        is_disk: true,
    };

    for attr in e.attributes().flatten() {
        match attr.key.as_ref() {
            b"name" => disk.name = attr_value(&attr)?.to_string(),
            b"md5" => disk.md5 = Some(attr_value(&attr)?.to_uppercase()),
            b"sha1" => disk.sha1 = Some(attr_value(&attr)?.to_uppercase()),
            b"status" => disk.status = RomStatus::parse(&attr_value(&attr)?),
            b"merge" => disk.merge = Some(attr_value(&attr)?.to_string()),
            _ => {}
        }
    }

    Ok(disk)
}

/// Decode and unescape an attribute value.
///
/// DAT files are UTF-8, so we decode the raw bytes directly and resolve the
/// predefined XML entities — matching the behaviour of quick-xml's former
/// `Attribute::unescape_value`, without the whitespace normalisation that
/// `normalized_value` would now introduce.
fn attr_value(attr: &quick_xml::events::attributes::Attribute) -> Result<String> {
    let raw = std::str::from_utf8(&attr.value).context("attribute value is not valid UTF-8")?;
    Ok(quick_xml::escape::unescape(raw)?.to_string())
}

/// Decode and unescape the text content of an element, matching quick-xml's
/// former `BytesText::unescape`.
fn text_value(e: &quick_xml::events::BytesText) -> Result<String> {
    let decoded = e.decode()?;
    Ok(quick_xml::escape::unescape(&decoded)?.to_string())
}

/// Read an element's full text content, concatenating every chunk until the
/// matching `</end>` tag, then trimming surrounding whitespace.
///
/// quick-xml emits each general entity reference (`&apos;`, `&amp;`, `&#39;`, …)
/// as its own [`Event::GeneralRef`], which splits one run of character data into
/// several events. Assigning on each event keeps only the final chunk — the bug
/// that stored "Shoot&apos;em Up" as "em Up". Accumulating across `Text`,
/// `CData`, and `GeneralRef` events reassembles the value faithfully.
fn read_element_text(reader: &mut Reader<&[u8]>, end: &[u8]) -> Result<String> {
    let mut buf = Vec::new();
    let mut out = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Text(e)) => out.push_str(&text_value(&e)?),
            Ok(Event::CData(e)) => out.push_str(&e.decode()?),
            Ok(Event::GeneralRef(e)) => out.push_str(&resolve_entity(&e)?),
            Ok(Event::End(e)) if e.name().as_ref() == end => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
        buf.clear();
    }
    Ok(out.trim().to_string())
}

/// Resolve a general entity reference to its text. Numeric references
/// (`&#39;`, `&#x27;`) decode directly; named references (`apos`, `amp`, `lt`,
/// `gt`, `quot`) are resolved through the predefined-entity table by
/// reconstructing `&name;` and unescaping it.
fn resolve_entity(e: &quick_xml::events::BytesRef) -> Result<String> {
    if let Some(ch) = e.resolve_char_ref()? {
        return Ok(ch.to_string());
    }
    let name = e.decode()?;
    Ok(quick_xml::escape::unescape(&format!("&{};", name))?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_dat() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>Nintendo - Nintendo Entertainment System</name>
    <description>Nintendo - Nintendo Entertainment System</description>
    <version>20231215-091234</version>
    <author>No-Intro</author>
  </header>
  <game name="Super Mario Bros. (World)">
    <description>Super Mario Bros. (World)</description>
    <rom name="Super Mario Bros. (World).nes" size="40976" crc="3337EC46" md5="811B027EAF99C2DEF7B933C5208636DE" sha1="FACEE9C577A5262DBE33AC4930BB0B58C8C037F7"/>
  </game>
</datafile>"#;

        let (header, games) = parse_dat(xml).unwrap();

        assert_eq!(header.name, "Nintendo - Nintendo Entertainment System");
        assert_eq!(header.version, Some("20231215-091234".to_string()));
        assert_eq!(header.author, Some("No-Intro".to_string()));

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "Super Mario Bros. (World)");
        assert_eq!(games[0].roms.len(), 1);
        assert_eq!(games[0].roms[0].name, "Super Mario Bros. (World).nes");
        assert_eq!(games[0].roms[0].size, 40976);
        assert_eq!(games[0].roms[0].crc32, Some("3337EC46".to_string()));
    }

    #[test]
    fn test_parse_non_self_closing_rom() {
        // `<rom>…</rom>` (not self-closing) must still be captured — MAME and
        // some tools emit this form. It was previously dropped silently, making
        // the game look ROM-less and therefore falsely "complete".
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>Test</name></header>
  <game name="game1">
    <description>Game One</description>
    <rom name="a.rom" size="1024" sha1="ABC123"></rom>
    <rom name="b.rom" size="2048" crc="DEADBEEF"/>
  </game>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();
        assert_eq!(games.len(), 1);
        // Both the non-self-closing and the self-closing rom are captured.
        assert_eq!(games[0].roms.len(), 2);
        assert_eq!(games[0].roms[0].name, "a.rom");
        assert_eq!(games[0].roms[0].sha1, Some("ABC123".to_string()));
        assert_eq!(games[0].roms[1].name, "b.rom");
        // Sibling text elements still parse correctly alongside the change.
        assert_eq!(games[0].description, Some("Game One".to_string()));
    }

    #[test]
    fn test_entity_references_in_text_are_not_truncated() {
        // Regression: quick-xml emits each entity reference as its own event,
        // splitting a text run. The header name "Shoot&apos;em Up" arrives as
        // Text("Commodore C64 - Games - Shoot") + GeneralRef("apos") +
        // Text("em Up - [D64]"); the old assign-per-event logic kept only the
        // last chunk ("em Up - [D64]"). It must reassemble the whole value.
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>Commodore C64 - Games - Shoot&apos;em Up - [D64]</name>
    <description>Nintendo Famicom &amp; Entertainment System</description>
  </header>
  <game name="g1">
    <description>Smash &#39;Em &amp; Run &#x21;</description>
    <rom name="a.rom" size="1" sha1="AA"/>
  </game>
</datafile>"#;

        let (header, games) = parse_dat(xml).unwrap();
        assert_eq!(header.name, "Commodore C64 - Games - Shoot'em Up - [D64]");
        // Spaces on either side of the entity must survive (no per-fragment trim).
        assert_eq!(
            header.description,
            Some("Nintendo Famicom & Entertainment System".to_string())
        );
        // Numeric (&#39;), named (&amp;), and hex (&#x21;) refs all resolve.
        assert_eq!(games[0].description, Some("Smash 'Em & Run !".to_string()));
    }

    #[test]
    fn test_detect_source_type() {
        let header = DatHeader {
            name: "Nintendo - NES".to_string(),
            author: Some("No-Intro".to_string()),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&header), DatSourceType::NoIntro);

        let header = DatHeader {
            name: "MAME 0.261".to_string(),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&header), DatSourceType::Mame);
    }

    #[test]
    fn test_parse_mame_machine_element() {
        // MAME uses <machine> instead of <game>
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>MAME 0.261</name>
    <version>0.261</version>
  </header>
  <machine name="pacman" sourcefile="pacman/pacman.cpp">
    <description>Pac-Man (Midway)</description>
    <rom name="pacman.6e" size="4096" crc="c1e6ab10" sha1="e87e059c5be45753f7e9f33dff851f16d6751181"/>
    <rom name="pacman.6f" size="4096" crc="1a6fb2d4" sha1="674d3a7f00d8be5e38b1fdc208ebef5a92d38329"/>
  </machine>
</datafile>"#;

        let (header, games) = parse_dat(xml).unwrap();

        assert_eq!(header.name, "MAME 0.261");
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "pacman");
        assert_eq!(games[0].roms.len(), 2);
    }

    #[test]
    fn test_parse_clone_relationship() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>Test</name></header>
  <game name="pacman">
    <description>Pac-Man</description>
    <rom name="pacman.rom" size="1000" sha1="ABC123"/>
  </game>
  <game name="puckman" cloneof="pacman" romof="pacman">
    <description>Puck Man (Japan)</description>
    <rom name="puckman.rom" size="1000" sha1="DEF456"/>
  </game>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();

        assert_eq!(games.len(), 2);

        // Parent
        assert_eq!(games[0].name, "pacman");
        assert!(games[0].clone_of.is_none());

        // Clone
        assert_eq!(games[1].name, "puckman");
        assert_eq!(games[1].clone_of, Some("pacman".to_string()));
        assert_eq!(games[1].rom_of, Some("pacman".to_string()));
    }

    #[test]
    fn test_parse_bios_and_device_flags() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>MAME</name></header>
  <machine name="neogeo" isbios="yes">
    <description>Neo-Geo</description>
    <rom name="neogeo.rom" size="524288" sha1="ABC"/>
  </machine>
  <machine name="neogeo_cart" isdevice="yes">
    <description>Neo-Geo Cartridge Slot</description>
  </machine>
  <machine name="pinball" ismechanical="yes">
    <description>Pinball Machine</description>
  </machine>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();

        assert_eq!(games.len(), 3);
        assert!(games[0].is_bios);
        assert!(!games[0].is_device);

        assert!(games[1].is_device);
        assert!(!games[1].is_bios);

        assert!(games[2].is_mechanical);
    }

    #[test]
    fn test_parse_rom_status() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>Test</name></header>
  <game name="test">
    <rom name="good.rom" size="100" sha1="A" status="good"/>
    <rom name="bad.rom" size="100" sha1="B" status="baddump"/>
    <rom name="no.rom" size="100" status="nodump"/>
  </game>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();

        assert_eq!(games[0].roms.len(), 3);
        assert_eq!(games[0].roms[0].status, RomStatus::Good);
        assert_eq!(games[0].roms[1].status, RomStatus::BadDump);
        assert_eq!(games[0].roms[2].status, RomStatus::NoDump);
    }

    #[test]
    fn test_parse_rom_merge_attribute() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>MAME</name></header>
  <game name="puckman" cloneof="pacman">
    <rom name="pacman.6e" merge="pacman.6e" size="4096" crc="c1e6ab10"/>
    <rom name="prg1" size="2048" crc="abcd1234"/>
  </game>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();

        assert_eq!(games[0].roms[0].merge, Some("pacman.6e".to_string()));
        assert!(games[0].roms[1].merge.is_none());
    }

    #[test]
    fn test_parse_multiple_games() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>Nintendo - NES</name></header>
  <game name="Zelda"><rom name="zelda.nes" size="1000" sha1="A"/></game>
  <game name="Mario"><rom name="mario.nes" size="2000" sha1="B"/></game>
  <game name="Metroid"><rom name="metroid.nes" size="3000" sha1="C"/></game>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();

        assert_eq!(games.len(), 3);
        assert_eq!(games[0].name, "Zelda");
        assert_eq!(games[1].name, "Mario");
        assert_eq!(games[2].name, "Metroid");
    }

    #[test]
    fn test_parse_game_with_multiple_roms() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>Test</name></header>
  <game name="multi_rom_game">
    <description>Game with multiple ROMs</description>
    <rom name="prg0" size="16384" sha1="A"/>
    <rom name="prg1" size="16384" sha1="B"/>
    <rom name="chr0" size="8192" sha1="C"/>
  </game>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();

        assert_eq!(games[0].roms.len(), 3);
        assert_eq!(games[0].roms[0].name, "prg0");
        assert_eq!(games[0].roms[1].name, "prg1");
        assert_eq!(games[0].roms[2].name, "chr0");
    }

    #[test]
    fn test_parse_disk_element() {
        // A MAME CHD DAT: <machine> with a self-closing <disk> carrying name +
        // sha1 (the CHD's internal hash), no size or crc. It must register as a
        // disk-flagged entry, not be dropped.
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>MAME CHDs</name></header>
  <machine name="azumanga">
    <description>Azumanga Daioh Puzzle Bobble</description>
    <disk name="gdl-0018" sha1="749a56dd64ab697f17470d8ae797f7e20e9eb646"/>
  </machine>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();

        assert_eq!(games[0].roms.len(), 1);
        let disk = &games[0].roms[0];
        assert!(disk.is_disk, "disk entry flagged");
        assert_eq!(disk.name, "gdl-0018");
        assert_eq!(
            disk.sha1.as_deref(),
            Some("749A56DD64AB697F17470D8AE797F7E20E9EB646")
        );
        assert_eq!(disk.size, 0);
        assert!(disk.crc32.is_none());
    }

    #[test]
    fn test_parse_rom_is_not_disk() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>Test</name></header>
  <game name="g"><rom name="a" size="16" sha1="A"/></game>
</datafile>"#;
        let (_, games) = parse_dat(xml).unwrap();
        assert!(!games[0].roms[0].is_disk, "a <rom> is not a disk");
    }

    #[test]
    fn test_parse_empty_header_fields() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>Minimal DAT</name>
  </header>
</datafile>"#;

        let (header, games) = parse_dat(xml).unwrap();

        assert_eq!(header.name, "Minimal DAT");
        assert!(header.version.is_none());
        assert!(header.author.is_none());
        assert!(header.description.is_none());
        assert_eq!(games.len(), 0);
    }

    #[test]
    fn test_parse_hash_case_normalization() {
        // Hashes should be uppercase
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>Test</name></header>
  <game name="test">
    <rom name="test.rom" size="100" crc="abcd1234" md5="deadbeef" sha1="cafebabe"/>
  </game>
</datafile>"#;

        let (_, games) = parse_dat(xml).unwrap();

        assert_eq!(games[0].roms[0].crc32, Some("ABCD1234".to_string()));
        assert_eq!(games[0].roms[0].md5, Some("DEADBEEF".to_string()));
        assert_eq!(games[0].roms[0].sha1, Some("CAFEBABE".to_string()));
    }

    #[test]
    fn test_detect_all_source_types() {
        // No-Intro
        let h = DatHeader {
            author: Some("No-Intro".to_string()),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&h), DatSourceType::NoIntro);

        // Redump
        let h = DatHeader {
            author: Some("redump.org".to_string()),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&h), DatSourceType::Redump);

        // TOSEC - by name
        let h = DatHeader {
            name: "TOSEC - Commodore Amiga".to_string(),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&h), DatSourceType::Tosec);

        // TOSEC - by homepage (real TOSEC DATs use this)
        let h = DatHeader {
            name: "Nintendo Famicom - Games".to_string(),
            homepage: Some("TOSEC".to_string()),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&h), DatSourceType::Tosec);

        // TOSEC - by category
        let h = DatHeader {
            name: "Commodore Amiga - Games".to_string(),
            category: Some("TOSEC".to_string()),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&h), DatSourceType::Tosec);

        // MAME
        let h = DatHeader {
            name: "MAME".to_string(),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&h), DatSourceType::Mame);

        // Custom/Unknown
        let h = DatHeader {
            name: "My Custom DAT".to_string(),
            ..Default::default()
        };
        assert_eq!(DatSourceType::detect(&h), DatSourceType::Custom);
    }

    #[test]
    fn test_rom_status_from_str() {
        assert_eq!(RomStatus::parse("good"), RomStatus::Good);
        assert_eq!(RomStatus::parse("baddump"), RomStatus::BadDump);
        assert_eq!(RomStatus::parse("nodump"), RomStatus::NoDump);
        assert_eq!(RomStatus::parse("BADDUMP"), RomStatus::BadDump);
        assert_eq!(RomStatus::parse("unknown"), RomStatus::Good); // default
    }

    #[test]
    fn test_rom_status_as_str() {
        assert_eq!(RomStatus::Good.as_str(), "good");
        assert_eq!(RomStatus::BadDump.as_str(), "baddump");
        assert_eq!(RomStatus::NoDump.as_str(), "nodump");
    }

    #[test]
    fn test_dat_source_type_as_str() {
        assert_eq!(DatSourceType::NoIntro.as_str(), "nointro");
        assert_eq!(DatSourceType::Redump.as_str(), "redump");
        assert_eq!(DatSourceType::Tosec.as_str(), "tosec");
        assert_eq!(DatSourceType::Mame.as_str(), "mame");
        assert_eq!(DatSourceType::Custom.as_str(), "custom");
    }
}
