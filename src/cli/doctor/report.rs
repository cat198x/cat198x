/// Health check result.
#[derive(Debug)]
pub(super) struct Check {
    name: String,
    status: CheckStatus,
    details: Option<String>,
}

#[derive(Debug, PartialEq)]
enum CheckStatus {
    Ok,
    Warning,
    Error,
}

impl Check {
    pub(super) fn ok(name: &str) -> Self {
        Self {
            name: name.to_string(),
            status: CheckStatus::Ok,
            details: None,
        }
    }

    pub(super) fn warning(name: &str, details: &str) -> Self {
        Self {
            name: name.to_string(),
            status: CheckStatus::Warning,
            details: Some(details.to_string()),
        }
    }

    pub(super) fn error(name: &str, details: &str) -> Self {
        Self {
            name: name.to_string(),
            status: CheckStatus::Error,
            details: Some(details.to_string()),
        }
    }

    fn status_icon(&self) -> &str {
        match self.status {
            CheckStatus::Ok => "[OK]",
            CheckStatus::Warning => "[WARN]",
            CheckStatus::Error => "[ERR]",
        }
    }
}

pub(super) fn print_report(checks: &[Check], fix: bool) {
    println!("Cat198x Health Check");
    println!("=====================\n");

    let mut errors = 0;
    let mut warnings = 0;

    for check in checks {
        let status_str = check.status_icon();
        print!("{} {}", status_str, check.name);

        if let Some(details) = &check.details {
            print!(": {}", details);
        }
        println!();

        match check.status {
            CheckStatus::Error => errors += 1,
            CheckStatus::Warning => warnings += 1,
            CheckStatus::Ok => {}
        }
    }

    println!();

    if errors > 0 {
        println!("Found {} error(s) and {} warning(s)", errors, warnings);
        if !fix {
            println!("Run with --fix to attempt automatic repairs");
        }
    } else if warnings > 0 {
        println!("Found {} warning(s)", warnings);
        if !fix {
            println!("Run with --fix to attempt automatic repairs");
        }
    } else {
        println!("All checks passed!");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_ok() {
        let check = Check::ok("Test check");
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.details.is_none());
    }

    #[test]
    fn test_check_warning() {
        let check = Check::warning("Test check", "Some warning");
        assert_eq!(check.status, CheckStatus::Warning);
        assert_eq!(check.details.as_deref(), Some("Some warning"));
    }

    #[test]
    fn test_check_error() {
        let check = Check::error("Test check", "Some error");
        assert_eq!(check.status, CheckStatus::Error);
        assert_eq!(check.details.as_deref(), Some("Some error"));
    }

    #[test]
    fn test_status_icons() {
        assert_eq!(Check::ok("").status_icon(), "[OK]");
        assert_eq!(Check::warning("", "").status_icon(), "[WARN]");
        assert_eq!(Check::error("", "").status_icon(), "[ERR]");
    }
}
