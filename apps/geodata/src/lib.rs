//! The MyOTA geodata catalogue service.
//!
//! This is scaffolding from chunk F1: it builds and its one function is tested,
//! so the toolchain and the deployment shape can be exercised before any
//! behaviour exists. A7 replaces this with a working service.
//!
//! **This service owns the `myota_geo` database.** No other service may connect to
//! it, and this service connects to no other service's database — see ADR-0001.

/// The service name, used in logs and in the status line.
///
/// It matches the image name and the Kubernetes workload prefix, so a log line
/// names the same thing that `kubectl` and the registry do.
pub const SERVICE_NAME: &str = "geodata";

/// Returns the one line the scaffold prints.
///
/// It is a pure function rather than an inline print so a table test can pin the
/// behaviour, and so the eventual replacement has an obvious seam.
#[must_use]
pub fn describe(args: &[String]) -> String {
    let wants_version = args.iter().any(|arg| arg == "--version");
    if wants_version {
        return format!("{SERVICE_NAME} (scaffold)");
    }
    format!("{SERVICE_NAME}: scaffold only; behaviour arrives in chunk A7")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_without_arguments_reports_the_scaffold() {
        assert_eq!(
            describe(&[]),
            "geodata: scaffold only; behaviour arrives in chunk A7"
        );
    }

    #[test]
    fn describe_with_version_flag_reports_the_version_line() {
        let args = vec!["--version".to_string()];
        assert_eq!(describe(&args), "geodata (scaffold)");
    }

    #[test]
    fn describe_ignores_unrelated_arguments() {
        let args = vec!["--verbose".to_string()];
        assert_eq!(
            describe(&args),
            "geodata: scaffold only; behaviour arrives in chunk A7"
        );
    }
}
