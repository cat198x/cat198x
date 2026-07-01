use std::fs;
use std::path::{Path, PathBuf};

pub fn create_test_dat(dir: &Path, name: &str) -> PathBuf {
    let dat_path = dir.join(format!("{name}.dat"));
    let content = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>{}</name>
    <description>{} (Test)</description>
    <version>20231215</version>
    <author>Test Author</author>
  </header>
  <game name="Test Game 1">
    <description>Test Game 1</description>
    <rom name="game1.rom" size="1024" crc="12345678" md5="D41D8CD98F00B204E9800998ECF8427E" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"/>
  </game>
  <game name="Test Game 2">
    <description>Test Game 2</description>
    <rom name="game2.rom" size="2048" crc="ABCDEF01" md5="098F6BCD4621D373CADE4E832627B4F6" sha1="A94A8FE5CCB19BA61C4C0873D391E987982FBBD3"/>
  </game>
</datafile>"#,
        name, name
    );
    write_dat(dat_path, content)
}

pub fn create_clrmamepro_dat(dir: &Path, name: &str) -> PathBuf {
    let dat_path = dir.join(format!("{name}.dat"));
    let content = format!(
        r#"clrmamepro (
    name "{}"
    description "{} (Test)"
    version "20231215"
    author "Test Author"
)

game (
    name "Test Game 1"
    description "Test Game 1"
    rom ( name "game1.rom" size 1024 crc 12345678 md5 D41D8CD98F00B204E9800998ECF8427E sha1 DA39A3EE5E6B4B0D3255BFEF95601890AFD80709 )
)

game (
    name "Test Game 2"
    description "Test Game 2"
    rom ( name "game2.rom" size 2048 crc ABCDEF01 md5 098F6BCD4621D373CADE4E832627B4F6 sha1 A94A8FE5CCB19BA61C4C0873D391E987982FBBD3 )
)
"#,
        name, name
    );
    write_dat(dat_path, content)
}

pub fn create_matching_dat(dir: &Path, name: &str, content_sha1: &str) -> PathBuf {
    let dat_path = dir.join(format!("{name}.dat"));
    let content = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>{}</name>
    <description>{} (Test)</description>
    <version>1.0</version>
    <author>Test</author>
  </header>
  <game name="Test Game">
    <description>Test Game</description>
    <rom name="test.rom" size="5" sha1="{}"/>
  </game>
</datafile>"#,
        name, name, content_sha1
    );
    write_dat(dat_path, content)
}

pub fn create_multi_rom_dat(dir: &Path, name: &str, roms: &[(&str, &str)]) -> PathBuf {
    let dat_path = dir.join(format!("{name}.dat"));

    let mut games_xml = String::new();
    for (i, (rom_name, sha1)) in roms.iter().enumerate() {
        games_xml.push_str(&format!(
            r#"  <game name="Game {}">
    <description>Game {}</description>
    <rom name="{}" size="5" sha1="{}"/>
  </game>
"#,
            i + 1,
            i + 1,
            rom_name,
            sha1
        ));
    }

    let content = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>{}</name>
    <description>{}</description>
    <version>1.0</version>
    <author>Test</author>
  </header>
{}
</datafile>"#,
        name, name, games_xml
    );
    write_dat(dat_path, content)
}

pub fn create_versioned_dat(dir: &Path, name: &str, version: &str) -> PathBuf {
    let dat_path = dir.join(format!("{}_{}.dat", name.replace(' ', "_"), version));
    let content = format!(
        r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
  <header>
    <name>{}</name>
    <description>{} (Test)</description>
    <version>{}</version>
    <author>Test Author</author>
  </header>
  <game name="Test Game 1">
    <description>Test Game 1</description>
    <rom name="game1.rom" size="1024" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"/>
  </game>
</datafile>"#,
        name, name, version
    );
    write_dat(dat_path, content)
}

fn write_dat(dat_path: PathBuf, content: String) -> PathBuf {
    if let Some(parent) = dat_path.parent() {
        fs::create_dir_all(parent).expect("failed to create DAT parent directory");
    }
    fs::write(&dat_path, content).expect("failed to write DAT file");
    dat_path
}
