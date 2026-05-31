//! ROM header detection and stripping
//!
//! Many ROM formats include headers added by dumping devices or emulators
//! that aren't part of the original cartridge data. No-Intro DATs hash
//! the headerless ROM data, so we need to detect and skip these headers
//! when computing hashes for matching.
//!
//! Supported formats:
//! - iNES/NES 2.0 (NES) - 16 bytes, magic: "NES\x1A"
//! - FDS (Famicom Disk System) - 16 bytes, magic: "FDS\x1A"
//! - A78 (Atari 7800) - 128 bytes, magic: "ATARI7800"
//! - LNX (Atari Lynx) - 64 bytes, magic: "LYNX"
//! - SMC (SNES copier) - 512 bytes, detected by file size

/// Detected ROM header information
#[derive(Debug, Clone, PartialEq)]
pub struct RomHeader {
    /// Header format name
    pub format: HeaderFormat,
    /// Number of bytes to skip for headerless hash
    pub skip_bytes: usize,
}

/// Known ROM header formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFormat {
    /// iNES format (NES) - 16 bytes
    INes,
    /// NES 2.0 format - 16 bytes
    Nes2,
    /// FDS format (Famicom Disk System) - 16 bytes
    Fds,
    /// A78 format (Atari 7800) - 128 bytes
    A78,
    /// LNX format (Atari Lynx) - 64 bytes
    Lnx,
    /// SMC format (SNES copier) - 512 bytes
    Smc,
}

impl HeaderFormat {
    /// Get the header size in bytes
    pub fn size(&self) -> usize {
        match self {
            HeaderFormat::INes | HeaderFormat::Nes2 | HeaderFormat::Fds => 16,
            HeaderFormat::Lnx => 64,
            HeaderFormat::A78 => 128,
            HeaderFormat::Smc => 512,
        }
    }

    /// Get the format name
    pub fn name(&self) -> &'static str {
        match self {
            HeaderFormat::INes => "iNES",
            HeaderFormat::Nes2 => "NES 2.0",
            HeaderFormat::Fds => "FDS",
            HeaderFormat::A78 => "A78",
            HeaderFormat::Lnx => "LNX",
            HeaderFormat::Smc => "SMC",
        }
    }
}

/// Detect if a ROM file has a header that should be skipped for hashing
///
/// Returns `Some(RomHeader)` if a header is detected, `None` otherwise.
///
/// # Arguments
/// * `data` - The first bytes of the ROM file (at least 512 bytes recommended)
/// * `file_size` - Total file size (used for SMC detection)
/// * `extension` - File extension (lowercase, without dot) for context
pub fn detect_header(data: &[u8], file_size: u64, extension: &str) -> Option<RomHeader> {
    // Need at least 16 bytes to detect any header
    if data.len() < 16 {
        return None;
    }

    // iNES / NES 2.0 header: "NES\x1A" at offset 0
    if data.len() >= 16 && &data[0..4] == b"NES\x1a" {
        // Check for NES 2.0 (byte 7 bits 2-3 == 2)
        let is_nes2 = (data[7] & 0x0C) == 0x08;
        return Some(RomHeader {
            format: if is_nes2 { HeaderFormat::Nes2 } else { HeaderFormat::INes },
            skip_bytes: 16,
        });
    }

    // FDS header: "FDS\x1A" at offset 0
    if data.len() >= 16 && &data[0..4] == b"FDS\x1a" {
        return Some(RomHeader {
            format: HeaderFormat::Fds,
            skip_bytes: 16,
        });
    }

    // A78 header: "ATARI7800" at offset 1 (byte 0 is version)
    if data.len() >= 128 && &data[1..10] == b"ATARI7800" {
        return Some(RomHeader {
            format: HeaderFormat::A78,
            skip_bytes: 128,
        });
    }

    // LNX header: "LYNX" at offset 0
    if data.len() >= 64 && &data[0..4] == b"LYNX" {
        return Some(RomHeader {
            format: HeaderFormat::Lnx,
            skip_bytes: 64,
        });
    }

    // SMC header: 512-byte copier header for SNES
    // Detected by: file size mod 1024 == 512 AND extension is .smc/.swc/.fig
    // AND file is large enough to have meaningful content after header
    if matches!(extension, "smc" | "swc" | "fig") && file_size > 512 && file_size % 1024 == 512 {
        return Some(RomHeader {
            format: HeaderFormat::Smc,
            skip_bytes: 512,
        });
    }

    None
}

/// Compute the offset to start hashing from (skipping any detected header)
///
/// This is a convenience function that returns 0 if no header is detected.
pub fn get_hash_offset(data: &[u8], file_size: u64, extension: &str) -> usize {
    detect_header(data, file_size, extension)
        .map(|h| h.skip_bytes)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_ines_header() {
        // iNES magic: NES\x1A followed by PRG/CHR sizes and flags
        let data = [
            0x4E, 0x45, 0x53, 0x1A, // "NES\x1A"
            0x02, 0x01, 0x01, 0x00, // PRG=2, CHR=1, flags
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        let header = detect_header(&data, 32784, "nes").unwrap();
        assert_eq!(header.format, HeaderFormat::INes);
        assert_eq!(header.skip_bytes, 16);
    }

    #[test]
    fn test_detect_nes2_header() {
        // NES 2.0: byte 7 bits 2-3 == 2 (value 0x08)
        let data = [
            0x4E, 0x45, 0x53, 0x1A,
            0x02, 0x01, 0x01, 0x08, // NES 2.0 flag in byte 7
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        let header = detect_header(&data, 32784, "nes").unwrap();
        assert_eq!(header.format, HeaderFormat::Nes2);
        assert_eq!(header.skip_bytes, 16);
    }

    #[test]
    fn test_detect_fds_header() {
        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(b"FDS\x1a");

        let header = detect_header(&data, 65536, "fds").unwrap();
        assert_eq!(header.format, HeaderFormat::Fds);
        assert_eq!(header.skip_bytes, 16);
    }

    #[test]
    fn test_detect_a78_header() {
        let mut data = [0u8; 128];
        // A78: version byte at 0, "ATARI7800" at offset 1
        data[0] = 0x01; // version
        data[1..10].copy_from_slice(b"ATARI7800");

        let header = detect_header(&data, 32896, "a78").unwrap();
        assert_eq!(header.format, HeaderFormat::A78);
        assert_eq!(header.skip_bytes, 128);
    }

    #[test]
    fn test_detect_lnx_header() {
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(b"LYNX");

        let header = detect_header(&data, 262208, "lnx").unwrap();
        assert_eq!(header.format, HeaderFormat::Lnx);
        assert_eq!(header.skip_bytes, 64);
    }

    #[test]
    fn test_detect_smc_header() {
        // SMC: 512 byte header, file size mod 1024 == 512
        let data = [0u8; 512];

        // File size: 512 (header) + 4194304 (4MB ROM) = 4194816
        // 4194816 % 1024 = 512 ✓
        let header = detect_header(&data, 4194816, "smc").unwrap();
        assert_eq!(header.format, HeaderFormat::Smc);
        assert_eq!(header.skip_bytes, 512);
    }

    #[test]
    fn test_no_smc_header_for_sfc() {
        // .sfc files don't have SMC headers
        let data = [0u8; 512];
        let header = detect_header(&data, 4194816, "sfc");
        assert!(header.is_none());
    }

    #[test]
    fn test_no_smc_header_wrong_size() {
        // File size must be 512 mod 1024
        let data = [0u8; 512];
        // 4194304 % 1024 = 0 (no header)
        let header = detect_header(&data, 4194304, "smc");
        assert!(header.is_none());
    }

    #[test]
    fn test_no_header_detected() {
        // Random data, no magic bytes
        let data = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
                    0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F];

        let header = detect_header(&data, 32768, "bin");
        assert!(header.is_none());
    }

    #[test]
    fn test_get_hash_offset_with_header() {
        let data = [
            0x4E, 0x45, 0x53, 0x1A,
            0x02, 0x01, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        assert_eq!(get_hash_offset(&data, 32784, "nes"), 16);
    }

    #[test]
    fn test_get_hash_offset_without_header() {
        let data = [0u8; 16];
        assert_eq!(get_hash_offset(&data, 32768, "bin"), 0);
    }

    #[test]
    fn test_header_format_size() {
        assert_eq!(HeaderFormat::INes.size(), 16);
        assert_eq!(HeaderFormat::Nes2.size(), 16);
        assert_eq!(HeaderFormat::Fds.size(), 16);
        assert_eq!(HeaderFormat::Lnx.size(), 64);
        assert_eq!(HeaderFormat::A78.size(), 128);
        assert_eq!(HeaderFormat::Smc.size(), 512);
    }

    #[test]
    fn test_header_format_name() {
        assert_eq!(HeaderFormat::INes.name(), "iNES");
        assert_eq!(HeaderFormat::A78.name(), "A78");
    }
}
