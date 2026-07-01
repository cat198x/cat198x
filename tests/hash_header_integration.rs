//! Integration tests for file hashing and header detection.

use cat198x::cli;

mod support;
use support::{TestEnv, create_test_rom};

#[test]
fn file_hashing_correctness() {
    let env = TestEnv::new();
    env.init();

    create_test_rom(&env.roms_dir, "empty.rom", b"");

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Add {
            preserve: false,
            consume: false,
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("source add failed");

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let db = env.db();
    let conn = db.conn();

    let file =
        cat198x::db::files::get_file_by_sha1(conn, "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709")
            .expect("file lookup should succeed");

    let file = file.expect("empty file should be indexed with correct SHA1");
    assert_eq!(
        file.md5,
        Some("D41D8CD98F00B204E9800998ECF8427E".to_string())
    );
    assert_eq!(file.crc32, Some("00000000".to_string()));
    assert_eq!(file.size, 0);
}

#[test]
fn header_detection_ines() {
    use cat198x::scanner::{HeaderFormat, detect_header};

    let mut ines_data = vec![0x4E, 0x45, 0x53, 0x1A];
    ines_data.extend([
        0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);

    let header = detect_header(&ines_data, 32784, "nes").expect("should detect iNES header");
    assert_eq!(header.format, HeaderFormat::INes);
    assert_eq!(header.skip_bytes, 16);
}

#[test]
fn header_detection_a78() {
    use cat198x::scanner::{HeaderFormat, detect_header};

    let mut a78_data = vec![0x01];
    a78_data.extend(b"ATARI7800");
    a78_data.resize(128, 0x00);

    let header = detect_header(&a78_data, 32896, "a78").expect("should detect A78 header");
    assert_eq!(header.format, HeaderFormat::A78);
    assert_eq!(header.skip_bytes, 128);
}

#[test]
fn no_header_for_plain_rom() {
    use cat198x::scanner::detect_header;

    let rom_data = vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
        0x0F,
    ];

    let header = detect_header(&rom_data, 32768, "bin");
    assert!(header.is_none(), "should not detect header for plain ROM");
}
