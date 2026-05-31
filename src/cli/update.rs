//! Self-update command - update ROMShelf to the latest version

use anyhow::{Context, Result};

/// Current version from Cargo.toml
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub repository owner
const REPO_OWNER: &str = "romshelf";

/// GitHub repository name
const REPO_NAME: &str = "romshelf";

/// Check for updates and optionally install them
pub fn run(check_only: bool, force: bool) -> Result<()> {
    println!("ROMShelf v{}", VERSION);
    println!();

    if check_only {
        check_for_update()
    } else {
        perform_update(force)
    }
}

/// Check if a newer version is available
fn check_for_update() -> Result<()> {
    println!("Checking for updates...");

    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("romshelf")
        .current_version(VERSION)
        .build()
        .context("Failed to configure update checker")?
        .get_latest_release()
        .context("Failed to check for updates. Are you connected to the internet?")?;

    let latest_version = status.version.trim_start_matches('v');

    if is_newer_version(latest_version, VERSION) {
        println!("New version available: v{}", latest_version);
        println!("Current version: v{}", VERSION);
        println!();
        println!("Run 'romshelf update' to install the update.");

        // Show release notes if available
        if let Some(body) = &status.body {
            if !body.is_empty() {
                println!();
                println!("Release notes:");
                // Truncate to first 500 chars
                let notes = if body.len() > 500 {
                    format!("{}...", &body[..500])
                } else {
                    body.clone()
                };
                for line in notes.lines().take(10) {
                    println!("  {}", line);
                }
            }
        }
    } else {
        println!("You're running the latest version (v{})", VERSION);
    }

    Ok(())
}

/// Download and install the latest version
fn perform_update(force: bool) -> Result<()> {
    println!("Checking for updates...");

    let mut update_builder = self_update::backends::github::Update::configure();
    update_builder
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("romshelf")
        .show_download_progress(true)
        .current_version(VERSION);

    // Determine the target based on current platform
    let target = get_target_triple();
    if let Some(target) = target {
        update_builder.target(&target);
    }

    let updater = update_builder
        .build()
        .context("Failed to configure updater")?;

    // Check current version against latest
    let latest = updater
        .get_latest_release()
        .context("Failed to check for updates")?;

    let latest_version = latest.version.trim_start_matches('v');

    if !force && !is_newer_version(latest_version, VERSION) {
        println!("You're already running the latest version (v{})", VERSION);
        return Ok(());
    }

    if force && !is_newer_version(latest_version, VERSION) {
        println!("Forcing reinstall of v{}", latest_version);
    } else {
        println!("Updating from v{} to v{}", VERSION, latest_version);
    }

    println!();

    // Perform the update
    let status = updater
        .update()
        .context("Failed to update. You may need to update manually.")?;

    println!();

    match status {
        self_update::Status::UpToDate(v) => {
            println!("Already up to date (v{})", v);
        }
        self_update::Status::Updated(v) => {
            println!("Successfully updated to v{}", v);
            println!();
            println!("Restart romshelf to use the new version.");
        }
    }

    Ok(())
}

/// Compare two semver versions, returns true if `new` is newer than `current`
fn is_newer_version(new: &str, current: &str) -> bool {
    let parse_version = |v: &str| -> (u32, u32, u32) {
        let parts: Vec<&str> = v.trim_start_matches('v').split('.').collect();
        let major = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let patch = parts.get(2).and_then(|s| s.split('-').next()?.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    };

    let new_v = parse_version(new);
    let current_v = parse_version(current);

    new_v > current_v
}

/// Get the target triple for the current platform
fn get_target_triple() -> Option<String> {
    // Common release asset naming patterns
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return None;
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };

    // Common naming: romshelf-x86_64-linux, romshelf-aarch64-darwin, etc.
    Some(format!("{}-{}", arch, os))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_version() {
        assert!(is_newer_version("0.2.0", "0.1.0"));
        assert!(is_newer_version("0.1.1", "0.1.0"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(is_newer_version("v0.2.0", "0.1.0"));

        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.1.0", "0.2.0"));
        assert!(!is_newer_version("0.0.9", "0.1.0"));
    }

    #[test]
    fn test_is_newer_version_with_prerelease() {
        // Pre-release suffixes are stripped for comparison
        assert!(is_newer_version("0.2.0-beta", "0.1.0"));
        assert!(is_newer_version("0.2.0", "0.1.0-alpha"));
    }

    #[test]
    fn test_get_target_triple() {
        let target = get_target_triple();
        // Should return something on supported platforms
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        assert!(target.is_some());
    }
}
