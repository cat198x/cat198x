use anyhow::{Context, Result, bail};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use super::get_data_dir;

/// Write a hard-delete audit log under `<data_dir>/objects/<log_dir>`.
///
/// The file is created with create-new semantics so repeated runs cannot
/// overwrite an earlier audit trail with the same prefix and timestamp.
pub(crate) fn write_hard_delete_log(
    data_dir: Option<PathBuf>,
    log_dir: &str,
    prefix: &str,
    entries: &[String],
) -> Result<PathBuf> {
    let logs_dir = get_data_dir(data_dir)?.join("objects").join(log_dir);
    std::fs::create_dir_all(&logs_dir).with_context(|| {
        format!(
            "failed to create audit log directory {}",
            logs_dir.display()
        )
    })?;

    let contents = entries.join("\n");
    let stem = format!("{}-{}-{}", prefix, timestamp(), std::process::id());
    for attempt in 0..1000 {
        let filename = if attempt == 0 {
            format!("{stem}.txt")
        } else {
            format!("{stem}-{attempt}.txt")
        };
        let path = logs_dir.join(filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())
                    .with_context(|| format!("failed to write audit log {}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to create audit log {}", path.display()));
            }
        }
    }

    bail!(
        "failed to create a unique audit log in {} after 1000 attempts",
        logs_dir.display()
    )
}

fn timestamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_creates_unique_files_without_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec!["/one.rom".to_string(), "/two.rom".to_string()];

        let first =
            write_hard_delete_log(Some(tmp.path().to_path_buf()), "logs", "delete", &entries)
                .unwrap();
        let second =
            write_hard_delete_log(Some(tmp.path().to_path_buf()), "logs", "delete", &entries)
                .unwrap();

        assert_ne!(first, second);
        assert_eq!(
            std::fs::read_to_string(first).unwrap(),
            "/one.rom\n/two.rom"
        );
        assert_eq!(
            std::fs::read_to_string(second).unwrap(),
            "/one.rom\n/two.rom"
        );
    }

    #[test]
    fn audit_log_returns_error_when_log_parent_is_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("objects"), "not a directory").unwrap();

        let result = write_hard_delete_log(Some(tmp.path().to_path_buf()), "logs", "delete", &[]);

        assert!(result.is_err());
    }
}
