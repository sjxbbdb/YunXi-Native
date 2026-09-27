//! Read-only Linux host probes used by the native tool layer.
//!
//! This module intentionally does not execute through a shell. Every probe
//! uses a fixed executable and validated positional arguments, returns bounded
//! JSON, and treats a missing Linux utility as an explicit `unavailable`
//! result. Mutating operations (install, restart, write, sudo) are not part of
//! this first tool-layer slice.

use anyhow::{Result, bail};
use clap::Subcommand;
use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_PROCESS_LIMIT: usize = 40;
const MAX_PROCESS_LIMIT: usize = 200;
const DEFAULT_FILESYSTEM_ENTRY_LIMIT: usize = 100;
const MAX_FILESYSTEM_ENTRY_LIMIT: usize = 200;
const MAX_FILESYSTEM_PATH_BYTES: usize = 4096;
const MAX_FILESYSTEM_ENTRY_NAME_BYTES: usize = 256;

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum LinuxToolCommand {
    /// Print the stable read-only Linux tool catalog.
    Describe,
    /// Read user/system systemd status without changing service state.
    SystemdStatus {
        /// Optional unit name, for example `yunxi-linux.service`.
        #[arg(long)]
        unit: Option<String>,
    },
    /// Read one local manual page through a non-interactive pager.
    Man {
        /// Man topic such as `systemctl` or `fish`.
        topic: String,
    },
    /// Read a bounded process snapshot.
    Processes {
        /// Maximum number of process rows returned.
        #[arg(long, default_value_t = DEFAULT_PROCESS_LIMIT)]
        limit: usize,
    },
    /// Read local interface and listening-socket state.
    Network,
    /// Query the local pacman database or the configured sync databases.
    ///
    /// This intentionally exposes only pacman's read-only `--info` and
    /// `--search` modes.  Package installation, removal, upgrade, database
    /// refresh, and every other mutating mode stay outside this command.
    Pacman {
        /// Query metadata for an installed package (`pacman --info`).
        #[arg(long, conflicts_with = "search", required_unless_present = "search")]
        info: Option<String>,
        /// Search configured package databases (`pacman --search`).
        #[arg(long, conflicts_with = "info", required_unless_present = "info")]
        search: Option<String>,
    },
    /// Read a bounded, non-recursive summary of one directory.
    FilesystemSummary {
        /// Directory to inspect. No shell expansion is performed.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum number of directory entries to inspect.
        #[arg(long, default_value_t = DEFAULT_FILESYSTEM_ENTRY_LIMIT)]
        limit: usize,
    },
    /// Read a bounded, non-recursive list of one directory's entries.
    FilesystemList {
        /// Directory to inspect. No shell expansion is performed.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum number of directory entries to return.
        #[arg(long, default_value_t = DEFAULT_FILESYSTEM_ENTRY_LIMIT)]
        limit: usize,
    },
    /// Read POSIX-style filesystem capacity and usage for one path.
    DiskUsage {
        /// File or directory whose mounted filesystem should be inspected.
        /// No shell expansion is performed.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize)]
struct LinuxToolSpec {
    name: &'static str,
    description: &'static str,
    risk_class: &'static str,
    requires_root: bool,
    mutates_system: bool,
}

#[derive(Debug, Serialize)]
struct LinuxToolResult {
    tool: &'static str,
    status: &'static str,
    risk_class: &'static str,
    requires_root: bool,
    mutates_system: bool,
    available: bool,
    exit_code: Option<i32>,
    command: Vec<String>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Serialize)]
struct LinuxFilesystemSummary {
    visible_entries: usize,
    directories: usize,
    files: usize,
    symlinks: usize,
    other: usize,
    file_bytes: u64,
}

#[derive(Debug, Serialize)]
struct LinuxFilesystemEntry {
    name: String,
    kind: &'static str,
    size_bytes: Option<u64>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct LinuxFilesystemResult {
    tool: &'static str,
    operation: &'static str,
    status: &'static str,
    risk_class: &'static str,
    requires_root: bool,
    mutates_system: bool,
    available: bool,
    path: String,
    limit: usize,
    scanned_entries: usize,
    truncated: bool,
    summary: Option<LinuxFilesystemSummary>,
    entries: Vec<LinuxFilesystemEntry>,
    error: Option<String>,
}

struct BoundedDirectoryEntries {
    entries: Vec<(String, PathBuf)>,
    truncated: bool,
    error: Option<String>,
}

pub(crate) fn run(command: LinuxToolCommand) -> Result<()> {
    let value = match command {
        LinuxToolCommand::Describe => serde_json::to_value(specs())?,
        LinuxToolCommand::SystemdStatus { unit } => {
            if let Some(unit) = &unit {
                validate_token(unit, "systemd unit")?;
            }
            serde_json::to_value(run_systemd_status(unit.as_deref()))?
        }
        LinuxToolCommand::Man { topic } => {
            validate_token(&topic, "man topic")?;
            serde_json::to_value(run_man(&topic))?
        }
        LinuxToolCommand::Processes { limit } => {
            let limit = limit.clamp(1, MAX_PROCESS_LIMIT);
            serde_json::to_value(run_processes(limit))?
        }
        LinuxToolCommand::Network => serde_json::to_value(run_network())?,
        LinuxToolCommand::Pacman { info, search } => {
            if let Some(package) = info.as_deref() {
                validate_pacman_token(package, "pacman package")?;
                serde_json::to_value(run_pacman("--info", package))?
            } else if let Some(query) = search.as_deref() {
                validate_pacman_token(query, "pacman search")?;
                serde_json::to_value(run_pacman("--search", query))?
            } else {
                bail!("pacman requires exactly one of --info or --search")
            }
        }
        LinuxToolCommand::FilesystemSummary { path, limit } => {
            validate_filesystem_path(&path)?;
            serde_json::to_value(run_filesystem_summary(
                &path,
                limit.clamp(1, MAX_FILESYSTEM_ENTRY_LIMIT),
            ))?
        }
        LinuxToolCommand::FilesystemList { path, limit } => {
            validate_filesystem_path(&path)?;
            serde_json::to_value(run_filesystem_list(
                &path,
                limit.clamp(1, MAX_FILESYSTEM_ENTRY_LIMIT),
            ))?
        }
        LinuxToolCommand::DiskUsage { path } => {
            validate_filesystem_path(&path)?;
            serde_json::to_value(run_disk_usage(&path))?
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn specs() -> Vec<LinuxToolSpec> {
    vec![
        LinuxToolSpec {
            name: "linux.systemd_status",
            description: "Read systemd user/system status without changing service state.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.man",
            description: "Read a local man page without opening an interactive pager.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.processes",
            description: "Read a bounded process snapshot from the local host.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.network",
            description: "Read local interface and listening-socket state.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.pacman",
            description: "Read installed package metadata or search Arch package databases.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.filesystem_summary",
            description: "Read a bounded, non-recursive summary of one directory.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.filesystem_list",
            description: "Read a bounded, non-recursive list of one directory's entries.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.disk_usage",
            description: "Read filesystem capacity and usage for one explicit path.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
    ]
}

fn run_filesystem_summary(path: &Path, limit: usize) -> LinuxFilesystemResult {
    let operation = "directory_summary";
    let tool = "linux.filesystem_summary";
    let path_text = path.to_string_lossy().into_owned();
    let directory = bounded_directory_entries(path, limit);
    let BoundedDirectoryEntries {
        entries,
        truncated,
        error,
    } = directory;
    if let Some(error) = error {
        return filesystem_failure(tool, operation, path_text, limit, error);
    }

    let mut summary = LinuxFilesystemSummary {
        visible_entries: entries.len(),
        directories: 0,
        files: 0,
        symlinks: 0,
        other: 0,
        file_bytes: 0,
    };
    for (_, entry_path) in &entries {
        match fs::symlink_metadata(entry_path) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                if file_type.is_dir() {
                    summary.directories += 1;
                } else if file_type.is_file() {
                    summary.files += 1;
                    summary.file_bytes = summary.file_bytes.saturating_add(metadata.len());
                } else if file_type.is_symlink() {
                    summary.symlinks += 1;
                } else {
                    summary.other += 1;
                }
            }
            Err(_) => summary.other += 1,
        }
    }
    LinuxFilesystemResult {
        tool,
        operation,
        status: "ok",
        risk_class: "read_only",
        requires_root: false,
        mutates_system: false,
        available: true,
        path: path_text,
        limit,
        scanned_entries: summary.visible_entries,
        truncated,
        summary: Some(summary),
        entries: Vec::new(),
        error: None,
    }
}

fn run_filesystem_list(path: &Path, limit: usize) -> LinuxFilesystemResult {
    let operation = "directory_list";
    let tool = "linux.filesystem_list";
    let path_text = path.to_string_lossy().into_owned();
    let directory = bounded_directory_entries(path, limit);
    let BoundedDirectoryEntries {
        entries,
        truncated,
        error,
    } = directory;
    if let Some(error) = error {
        return filesystem_failure(tool, operation, path_text, limit, error);
    }
    let entries = entries
        .iter()
        .map(|(name, entry_path)| filesystem_entry(name, entry_path))
        .collect::<Vec<_>>();
    LinuxFilesystemResult {
        tool,
        operation,
        status: "ok",
        risk_class: "read_only",
        requires_root: false,
        mutates_system: false,
        available: true,
        path: path_text,
        limit,
        scanned_entries: entries.len(),
        truncated,
        summary: None,
        entries,
        error: None,
    }
}

fn run_disk_usage(path: &Path) -> LinuxToolResult {
    run_fixed_command(
        "df",
        vec![
            "-P".to_string(),
            "-k".to_string(),
            "--".to_string(),
            path.to_string_lossy().into_owned(),
        ],
        "linux.disk_usage",
    )
}

fn bounded_directory_entries(path: &Path, limit: usize) -> BoundedDirectoryEntries {
    let read_dir = match fs::read_dir(path) {
        Ok(read_dir) => read_dir,
        Err(error) => {
            return BoundedDirectoryEntries {
                entries: Vec::new(),
                truncated: false,
                error: Some(format!("unable to read directory: {error}")),
            };
        }
    };
    let mut entries = Vec::new();
    let mut error = None;
    for item in read_dir.take(limit.saturating_add(1)) {
        match item {
            Ok(entry) => {
                let name = bounded_string(
                    &entry.file_name().to_string_lossy(),
                    MAX_FILESYSTEM_ENTRY_NAME_BYTES,
                );
                entries.push((name, entry.path()));
            }
            Err(item_error) => {
                error = Some(format!("unable to read directory entry: {item_error}"));
                break;
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let truncated = entries.len() > limit;
    entries.truncate(limit);
    BoundedDirectoryEntries {
        entries,
        truncated,
        error,
    }
}

fn filesystem_entry(name: &str, path: &Path) -> LinuxFilesystemEntry {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            let (kind, size_bytes) = if file_type.is_dir() {
                ("directory", None)
            } else if file_type.is_file() {
                ("file", Some(metadata.len()))
            } else if file_type.is_symlink() {
                ("symlink", None)
            } else {
                ("other", None)
            };
            LinuxFilesystemEntry {
                name: name.to_string(),
                kind,
                size_bytes,
                error: None,
            }
        }
        Err(error) => LinuxFilesystemEntry {
            name: name.to_string(),
            kind: "unknown",
            size_bytes: None,
            error: Some(bounded_string(&error.to_string(), 256)),
        },
    }
}

fn filesystem_failure(
    tool: &'static str,
    operation: &'static str,
    path: String,
    limit: usize,
    error: String,
) -> LinuxFilesystemResult {
    LinuxFilesystemResult {
        tool,
        operation,
        status: "failed",
        risk_class: "read_only",
        requires_root: false,
        mutates_system: false,
        available: true,
        path,
        limit,
        scanned_entries: 0,
        truncated: false,
        summary: None,
        entries: Vec::new(),
        error: Some(bounded_string(&error, 1024)),
    }
}

fn run_systemd_status(unit: Option<&str>) -> LinuxToolResult {
    let mut args = vec!["--user".to_string(), "--no-pager".to_string()];
    if let Some(unit) = unit {
        args.extend([
            "status".to_string(),
            "--full".to_string(),
            "--plain".to_string(),
            "--".to_string(),
            unit.to_string(),
        ]);
    } else {
        args.push("is-system-running".to_string());
    }
    run_fixed_command("systemctl", args, "linux.systemd_status")
}

fn run_man(topic: &str) -> LinuxToolResult {
    let mut result = run_fixed_command(
        "man",
        vec![
            "-P".to_string(),
            "cat".to_string(),
            "--".to_string(),
            topic.to_string(),
        ],
        "linux.man",
    );
    // Man exits non-zero for an unavailable page; keep that fact in JSON but
    // make the user-facing error explicit and bounded.
    if result.status == "failed" && result.stderr.is_empty() {
        result.stderr = format!("man page not available: {topic}");
    }
    result
}

fn run_processes(limit: usize) -> LinuxToolResult {
    let mut result = run_fixed_command(
        "ps",
        vec![
            "-eo".to_string(),
            "pid=,ppid=,stat=,comm=,args=".to_string(),
            "--sort=-%cpu".to_string(),
        ],
        "linux.processes",
    );
    if result.available {
        result.stdout = result
            .stdout
            .lines()
            .take(limit)
            .collect::<Vec<_>>()
            .join("\n");
        if !result.stdout.is_empty() {
            result.stdout.push('\n');
        }
    }
    result
}

fn run_network() -> LinuxToolResult {
    let first = run_fixed_command(
        "ip",
        vec!["-brief".to_string(), "address".to_string()],
        "linux.network",
    );
    if first.available {
        return first;
    }
    run_fixed_command("ss", vec!["-tuln".to_string()], "linux.network")
}

fn run_pacman(mode: &'static str, value: &str) -> LinuxToolResult {
    run_fixed_command(
        "pacman",
        vec![mode.to_string(), "--".to_string(), value.to_string()],
        "linux.pacman",
    )
}

fn run_fixed_command(program: &str, args: Vec<String>, tool: &'static str) -> LinuxToolResult {
    let mut command = vec![program.to_string()];
    command.extend(args.iter().cloned());
    let output = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) => result_from_output(tool, command, output),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LinuxToolResult {
            tool,
            status: "unavailable",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
            available: false,
            exit_code: None,
            command,
            stdout: String::new(),
            stderr: format!("required Linux utility is unavailable: {program}"),
        },
        Err(error) => LinuxToolResult {
            tool,
            status: "failed",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
            available: true,
            exit_code: None,
            command,
            stdout: String::new(),
            stderr: format!("failed to start {program}: {error}"),
        },
    }
}

fn result_from_output(tool: &'static str, command: Vec<String>, output: Output) -> LinuxToolResult {
    let exit_code = output.status.code();
    LinuxToolResult {
        tool,
        status: if output.status.success() {
            "ok"
        } else {
            "failed"
        },
        risk_class: "read_only",
        requires_root: false,
        mutates_system: false,
        available: true,
        exit_code,
        command,
        stdout: bounded_text(&output.stdout),
        stderr: bounded_text(&output.stderr),
    }
}

fn bounded_text(bytes: &[u8]) -> String {
    let bytes = if bytes.len() > MAX_OUTPUT_BYTES {
        &bytes[..MAX_OUTPUT_BYTES]
    } else {
        bytes
    };
    String::from_utf8_lossy(bytes).into_owned()
}

fn validate_token(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '@' | '_' | ':' | '-')
        })
    {
        bail!("{label} contains unsupported characters")
    }
    Ok(())
}

fn validate_pacman_token(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '.' | '@' | '_' | ':' | '+' | '-' | '/' | '*' | '?'
                )
        })
    {
        bail!("{label} contains unsupported characters")
    }
    Ok(())
}

fn validate_filesystem_path(path: &Path) -> Result<()> {
    let text = path.to_string_lossy();
    if text.is_empty() || text.chars().any(char::is_control) {
        bail!("filesystem path is empty or contains control characters")
    }
    if text.len() > MAX_FILESYSTEM_PATH_BYTES {
        bail!("filesystem path exceeds {MAX_FILESYSTEM_PATH_BYTES} bytes")
    }
    for component in path.components() {
        if let Component::Normal(value) = component
            && value.to_string_lossy().len() > MAX_FILESYSTEM_ENTRY_NAME_BYTES
        {
            bail!("filesystem path component exceeds {MAX_FILESYSTEM_ENTRY_NAME_BYTES} bytes")
        }
    }
    Ok(())
}

fn bounded_string(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_read_only_and_non_root() {
        let specs = specs();
        assert_eq!(specs.len(), 8);
        assert!(specs.iter().all(|spec| {
            spec.risk_class == "read_only" && !spec.requires_root && !spec.mutates_system
        }));
    }

    #[test]
    fn tokens_reject_shell_syntax_and_whitespace() {
        assert!(validate_token("yunxi-linux.service", "unit").is_ok());
        assert!(validate_token("systemctl; rm -rf /", "unit").is_err());
        assert!(validate_token("systemctl --user", "topic").is_err());
    }

    #[test]
    fn pacman_queries_are_fixed_and_reject_shell_syntax() {
        assert!(validate_pacman_token("yunxi-agent", "package").is_ok());
        assert!(validate_pacman_token("linux-firmware*", "search").is_ok());
        assert!(validate_pacman_token("foo; touch /tmp/yunxi", "search").is_err());
        assert!(validate_pacman_token("pacman --info", "package").is_err());

        let result = run_pacman("--info", "yunxi-agent");
        assert_eq!(result.tool, "linux.pacman");
        assert_eq!(result.command, ["pacman", "--info", "--", "yunxi-agent"]);
        assert_eq!(result.risk_class, "read_only");
        assert!(!result.requires_root);
        assert!(!result.mutates_system);
    }

    #[test]
    fn output_is_bounded() {
        let output = bounded_text(&vec![b'a'; MAX_OUTPUT_BYTES + 10]);
        assert_eq!(output.len(), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn filesystem_snapshot_is_bounded_and_deterministic() {
        let root = std::env::temp_dir().join(format!(
            "yunxi-linux-filesystem-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("tempdir");
        std::fs::write(root.join("note.txt"), "hello").expect("file");
        std::fs::create_dir(root.join("nested")).expect("directory");
        let result = run_filesystem_list(&root, 1);
        assert_eq!(result.tool, "linux.filesystem_list");
        assert_eq!(result.status, "ok");
        assert_eq!(result.scanned_entries, 1);
        assert!(result.truncated);
        assert_eq!(result.entries[0].name, "nested");
        assert!(serde_json::to_vec(&result).expect("json").len() <= MAX_OUTPUT_BYTES);

        let summary = run_filesystem_summary(&root, 8);
        let summary = summary.summary.expect("summary");
        assert_eq!(summary.directories, 1);
        assert_eq!(summary.files, 1);
        assert_eq!(summary.file_bytes, 5);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn filesystem_paths_reject_nul_and_oversized_components() {
        assert!(validate_filesystem_path(Path::new("./safe")).is_ok());
        assert!(validate_filesystem_path(Path::new("bad\0path")).is_err());
        assert!(validate_filesystem_path(Path::new("bad\npath")).is_err());
        let oversized = "x".repeat(MAX_FILESYSTEM_ENTRY_NAME_BYTES + 1);
        assert!(validate_filesystem_path(Path::new(&oversized)).is_err());
    }

    #[test]
    fn disk_usage_uses_fixed_df_argv_and_preserves_path_as_one_argument() {
        let result = run_disk_usage(Path::new("/tmp/a path"));
        assert_eq!(result.tool, "linux.disk_usage");
        assert_eq!(result.command, ["df", "-P", "-k", "--", "/tmp/a path"]);
        assert_eq!(result.risk_class, "read_only");
        assert!(!result.requires_root);
        assert!(!result.mutates_system);
    }
}
