//! Stable ZIP filename decoding across zip crate upgrades.
//!
//! PKWARE APPNOTE 6.3.10, Appendix D.2: bit 11 selects UTF-8; otherwise
//! names use IBM code page 437. A validated Info-ZIP Unicode Path field is
//! already applied to the raw name and bit 11 by zip before we see the entry.
//! ZIP 9's name() instead guesses UTF-8 first, which changes existing catalogue
//! identities for unflagged CP437 bytes that are also valid UTF-8.

use std::borrow::Cow;
use std::io::Read;

/// Decode the same name during scanning, lookup and repacking.
pub(crate) fn zip_entry_name<'a, R: Read + ?Sized>(
    entry: &'a zip::read::ZipFile<'_, R>,
) -> Cow<'a, str> {
    decode_name(entry.name_raw(), entry.flags().as_u16() & (1 << 11) != 0)
}

fn decode_name(raw: &[u8], utf8: bool) -> Cow<'_, str> {
    if utf8 || raw.is_ascii() {
        // Match ZIP 8's treatment of malformed flagged UTF-8 names. Changing
        // their identity would strand previously catalogued entries too.
        String::from_utf8_lossy(raw)
    } else {
        Cow::Owned(
            raw.iter()
                .map(|&byte| {
                    if byte < 128 {
                        char::from(byte)
                    } else {
                        CP437_HIGH[usize::from(byte - 128)]
                    }
                })
                .collect(),
        )
    }
}

// IBM code page 437, bytes 80–FF. Mapping reference:
// https://www.unicode.org/Public/MAPPINGS/VENDORS/MICSFT/PC/CP437.TXT
const CP437_HIGH: [char; 128] = [
    '\u{00c7}', '\u{00fc}', '\u{00e9}', '\u{00e2}', '\u{00e4}', '\u{00e0}', '\u{00e5}', '\u{00e7}',
    '\u{00ea}', '\u{00eb}', '\u{00e8}', '\u{00ef}', '\u{00ee}', '\u{00ec}', '\u{00c4}', '\u{00c5}',
    '\u{00c9}', '\u{00e6}', '\u{00c6}', '\u{00f4}', '\u{00f6}', '\u{00f2}', '\u{00fb}', '\u{00f9}',
    '\u{00ff}', '\u{00d6}', '\u{00dc}', '\u{00a2}', '\u{00a3}', '\u{00a5}', '\u{20a7}', '\u{0192}',
    '\u{00e1}', '\u{00ed}', '\u{00f3}', '\u{00fa}', '\u{00f1}', '\u{00d1}', '\u{00aa}', '\u{00ba}',
    '\u{00bf}', '\u{2310}', '\u{00ac}', '\u{00bd}', '\u{00bc}', '\u{00a1}', '\u{00ab}', '\u{00bb}',
    '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}', '\u{2561}', '\u{2562}', '\u{2556}',
    '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255d}', '\u{255c}', '\u{255b}', '\u{2510}',
    '\u{2514}', '\u{2534}', '\u{252c}', '\u{251c}', '\u{2500}', '\u{253c}', '\u{255e}', '\u{255f}',
    '\u{255a}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}', '\u{2550}', '\u{256c}', '\u{2567}',
    '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}', '\u{2552}', '\u{2553}', '\u{256b}',
    '\u{256a}', '\u{2518}', '\u{250c}', '\u{2588}', '\u{2584}', '\u{258c}', '\u{2590}', '\u{2580}',
    '\u{03b1}', '\u{00df}', '\u{0393}', '\u{03c0}', '\u{03a3}', '\u{03c3}', '\u{00b5}', '\u{03c4}',
    '\u{03a6}', '\u{0398}', '\u{03a9}', '\u{03b4}', '\u{221e}', '\u{03c6}', '\u{03b5}', '\u{2229}',
    '\u{2261}', '\u{00b1}', '\u{2265}', '\u{2264}', '\u{2320}', '\u{2321}', '\u{00f7}', '\u{2248}',
    '\u{00b0}', '\u{2219}', '\u{00b7}', '\u{221a}', '\u{207f}', '\u{00b2}', '\u{25a0}', '\u{00a0}',
];

#[cfg(test)]
mod tests {
    use super::decode_name;

    #[test]
    fn encoding_flag_decides_ambiguous_bytes() {
        assert_eq!(decode_name(b"\xc3\xa9.bin", false), "├⌐.bin");
        assert_eq!(decode_name(b"\xc3\xa9.bin", true), "é.bin");
        assert_eq!(decode_name(b"\x86.bin", false), "å.bin");
    }

    #[test]
    fn malformed_flagged_utf8_keeps_previous_lossy_decoding() {
        assert_eq!(decode_name(b"\x86.bin", true), "�.bin");
    }
}
