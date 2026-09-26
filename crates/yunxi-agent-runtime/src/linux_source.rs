//! Bounded, read-only discovery of the local Linux source version.
//!
//! This module only reads the standard `os-release` metadata file. It never
//! invokes a shell or a package manager, and an unknown/invalid profile is
//! represented as `None` so callers can preserve unfiltered knowledge recall.

use std::fs::File;
use std::io::Read;

const MAX_OS_RELEASE_BYTES: usize = 16 * 1024;

pub(crate) fn detect_source_version() -> Option<String> {
    ["/etc/os-release", "/usr/lib/os-release"]
        .into_iter()
        .find_map(read_source_version)
}

fn read_source_version(path: &str) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_OS_RELEASE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_OS_RELEASE_BYTES {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    parse_source_version(text)
}

fn parse_source_version(text: &str) -> Option<String> {
    let id = metadata_value(text, "ID")?;
    let version = metadata_value(text, "VERSION_ID");
    if id == "arch" && version.is_none() {
        return Some("arch-rolling".to_string());
    }
    Some(format!("{id}-{}", version?))
}

fn metadata_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let raw = text
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))?
        .trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(raw)
        .to_ascii_lowercase();
    if value.is_empty()
        || value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || "._-".contains(character)))
    {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::parse_source_version;

    #[test]
    fn parses_versioned_distribution_metadata() {
        assert_eq!(
            parse_source_version("ID=Ubuntu\nVERSION_ID=\"24.04\"\n"),
            Some("ubuntu-24.04".to_string())
        );
    }

    #[test]
    fn identifies_arch_as_a_rolling_source_without_version_id() {
        assert_eq!(
            parse_source_version("ID=arch\nNAME=Arch Linux\n"),
            Some("arch-rolling".to_string())
        );
    }

    #[test]
    fn rejects_ambiguous_or_unsafe_metadata() {
        assert_eq!(parse_source_version("ID=ubuntu\n"), None);
        assert_eq!(
            parse_source_version("ID=ubuntu\nVERSION_ID=\"24.04\\n\"\n"),
            None
        );
    }
}
