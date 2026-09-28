//! Typed, approval-gated workspace file mutations for Linux.
//!
//! This module intentionally does not accept shell text.  It executes only
//! write, delete, and move operations on regular files, after the outer tool
//! policy has authorized the request.  Every operation is preflighted before
//! mutation and a small on-disk journal is written after success; a partial
//! failure is rolled back from the in-memory snapshots.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use yunxi_agent_sandbox::{ExecutionPolicy, SandboxRequirement};

const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum LinuxApplyInput {
    WriteFile { path: String, content: String },
    DeletePath { path: String },
    MovePath { from: String, to: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxApplyReport {
    pub schema_version: u32,
    pub operation: String,
    pub changed_paths: Vec<String>,
    pub journal_path: String,
    pub rolled_back: bool,
    pub change_kind: LinuxApplyChangeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxApplyChangeKind {
    Added,
    Updated,
    Deleted,
    Moved,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxUndoReport {
    pub schema_version: u32,
    pub restored_paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinuxUndoJournalSummary {
    pub journal_path: String,
    pub operation: String,
    pub entries: usize,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinuxUndoJournalList {
    pub journals: Vec<LinuxUndoJournalSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct LinuxApplyError(String);

impl LinuxApplyError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for LinuxApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LinuxApplyError {}

#[derive(Clone, Debug)]
struct Snapshot {
    path: PathBuf,
    content: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct JournalEntry {
    path: String,
    existed: bool,
    backup: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Journal {
    schema_version: u32,
    operation: String,
    entries: Vec<JournalEntry>,
}

pub fn apply(
    cwd: &Path,
    input: LinuxApplyInput,
    policy: &ExecutionPolicy,
) -> Result<LinuxApplyReport, LinuxApplyError> {
    let root = lexical_normalize(&policy.workspace_root);
    let workspace_write = matches!(policy.sandbox, SandboxRequirement::WorkspaceWrite);
    let danger = matches!(policy.sandbox, SandboxRequirement::DangerFullAccess);
    if matches!(policy.sandbox, SandboxRequirement::ReadOnly) {
        return Err(LinuxApplyError::new(
            "linux_apply requires WorkspaceWrite or DangerFullAccess sandbox",
        ));
    }

    let (operation, paths, content) = match input {
        LinuxApplyInput::WriteFile { path, content } => {
            validate_path_text(&path, "write path")?;
            if content.len() > MAX_CONTENT_BYTES {
                return Err(LinuxApplyError::new("write content exceeds 64 MiB"));
            }
            (
                "write_file".to_string(),
                vec![path],
                Some(content.into_bytes()),
            )
        }
        LinuxApplyInput::DeletePath { path } => {
            validate_path_text(&path, "delete path")?;
            ("delete_path".to_string(), vec![path], None)
        }
        LinuxApplyInput::MovePath { from, to } => {
            validate_path_text(&from, "move source")?;
            validate_path_text(&to, "move destination")?;
            ("move_path".to_string(), vec![from, to], None)
        }
    };

    let resolved = paths
        .iter()
        .map(|path| secure_path(cwd, &root, path, workspace_write || !danger))
        .collect::<Result<Vec<_>, _>>()?;
    for path in &resolved {
        reject_yunxi_state(&root, path)?;
    }

    let mut snapshots = Vec::new();
    for path in &resolved {
        snapshots.push(snapshot(path)?);
    }

    let undo_root = cwd.join(".yunxi").join("undo");
    fs::create_dir_all(&undo_root)
        .map_err(|error| LinuxApplyError::new(format!("create undo journal: {error}")))?;
    let token = journal_token();
    let journal_dir = undo_root.join(format!("linux-apply-{token}"));
    fs::create_dir(&journal_dir)
        .map_err(|error| LinuxApplyError::new(format!("create undo journal directory: {error}")))?;

    let result = execute_operation(&operation, &resolved, content.as_deref());
    if let Err(error) = result {
        let rollback = rollback(&snapshots);
        let _ = fs::remove_dir_all(&journal_dir);
        return Err(if rollback.is_ok() {
            error
        } else {
            LinuxApplyError::new(format!("{error}; rollback failed"))
        });
    }

    let journal_result = (|| -> Result<PathBuf, LinuxApplyError> {
        let mut entries = Vec::new();
        for (index, snapshot) in snapshots.iter().enumerate() {
            let backup = snapshot.content.as_ref().map(|content| {
                let name = format!("{index}.bak");
                let path = journal_dir.join(&name);
                (name, path, content)
            });
            let backup_name = if let Some((name, path, content)) = backup {
                let mut file = File::create(&path)
                    .map_err(|error| LinuxApplyError::new(format!("write undo backup: {error}")))?;
                file.write_all(content)
                    .map_err(|error| LinuxApplyError::new(format!("write undo backup: {error}")))?;
                Some(name)
            } else {
                None
            };
            entries.push(JournalEntry {
                path: display_relative(&root, &snapshot.path),
                existed: snapshot.content.is_some(),
                backup: backup_name,
            });
        }
        let journal = Journal {
            schema_version: 1,
            operation: operation.clone(),
            entries,
        };
        let journal_file = journal_dir.join("journal.json");
        let bytes = serde_json::to_vec_pretty(&journal)
            .map_err(|error| LinuxApplyError::new(format!("serialize undo journal: {error}")))?;
        fs::write(&journal_file, bytes)
            .map_err(|error| LinuxApplyError::new(format!("write undo journal: {error}")))?;
        Ok(journal_file)
    })();
    let journal_file = match journal_result {
        Ok(path) => path,
        Err(error) => {
            let rollback_result = rollback(&snapshots);
            let _ = fs::remove_dir_all(&journal_dir);
            return Err(if rollback_result.is_ok() {
                error
            } else {
                LinuxApplyError::new(format!("{error}; rollback failed"))
            });
        }
    };

    let change_kind = match operation.as_str() {
        "write_file" if snapshots[0].content.is_some() => LinuxApplyChangeKind::Updated,
        "write_file" => LinuxApplyChangeKind::Added,
        "delete_path" => LinuxApplyChangeKind::Deleted,
        "move_path" => LinuxApplyChangeKind::Moved,
        _ => LinuxApplyChangeKind::Updated,
    };
    Ok(LinuxApplyReport {
        schema_version: 1,
        operation,
        changed_paths: resolved
            .iter()
            .map(|path| display_relative(&root, path))
            .collect(),
        journal_path: journal_file.display().to_string(),
        rolled_back: false,
        change_kind,
    })
}

/// Restore one successful apply operation from its journal.
pub fn undo(journal_path: &Path) -> Result<LinuxUndoReport, LinuxApplyError> {
    if journal_path.file_name().and_then(|name| name.to_str()) != Some("journal.json") {
        return Err(LinuxApplyError::new("undo path must point to journal.json"));
    }
    let journal_dir = journal_path
        .parent()
        .ok_or_else(|| LinuxApplyError::new("undo journal has no parent"))?;
    if journal_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_none()
        || journal_dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some("undo")
        || journal_dir
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some(".yunxi")
    {
        return Err(LinuxApplyError::new(
            "undo journal is outside the YunXi undo directory",
        ));
    }
    let root = journal_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| LinuxApplyError::new("undo journal has no workspace root"))?;
    let bytes = fs::read(journal_path)
        .map_err(|error| LinuxApplyError::new(format!("read undo journal: {error}")))?;
    let journal: Journal = serde_json::from_slice(&bytes)
        .map_err(|error| LinuxApplyError::new(format!("parse undo journal: {error}")))?;
    let mut restored = Vec::new();
    for entry in journal.entries.iter().rev() {
        let target = secure_path(root, root, &entry.path, true)?;
        reject_yunxi_state(root, &target)?;
        if entry.existed {
            let backup = entry
                .backup
                .as_deref()
                .ok_or_else(|| LinuxApplyError::new("undo journal is missing a backup"))?;
            let backup_path = journal_dir.join(backup);
            if backup_path.parent() != Some(journal_dir) {
                return Err(LinuxApplyError::new(
                    "undo backup escapes its journal directory",
                ));
            }
            let content = fs::read(&backup_path)
                .map_err(|error| LinuxApplyError::new(format!("read undo backup: {error}")))?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    LinuxApplyError::new(format!("create undo parent: {error}"))
                })?;
            }
            fs::write(&target, content)
                .map_err(|error| LinuxApplyError::new(format!("restore file: {error}")))?;
        } else if target.exists() {
            fs::remove_file(&target)
                .map_err(|error| LinuxApplyError::new(format!("remove applied file: {error}")))?;
        }
        restored.push(entry.path.clone());
    }
    Ok(LinuxUndoReport {
        schema_version: 1,
        restored_paths: restored,
    })
}

/// List valid undo journals for a workspace without changing any files.
pub fn list_journals(cwd: &Path) -> Result<Vec<LinuxUndoJournalSummary>, LinuxApplyError> {
    Ok(list_journals_with_warnings(cwd)?.journals)
}

/// List valid journals and retain bounded diagnostics for entries that were
/// skipped. A malformed or symlinked entry must not hide healthy journals.
pub fn list_journals_with_warnings(cwd: &Path) -> Result<LinuxUndoJournalList, LinuxApplyError> {
    let undo_root = cwd.join(".yunxi").join("undo");
    let Ok(entries) = fs::read_dir(&undo_root) else {
        return Ok(LinuxUndoJournalList {
            journals: Vec::new(),
            warnings: Vec::new(),
        });
    };
    let mut journals = Vec::new();
    let mut warnings = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            LinuxApplyError::new(format!("read undo journal directory: {error}"))
        })?;
        let file_type = entry.file_type().map_err(|error| {
            LinuxApplyError::new(format!("inspect undo journal directory: {error}"))
        })?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path().join("journal.json");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                warnings.push(format!(
                    "skipped {}: inspect failed: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            warnings.push(format!(
                "skipped {}: journal.json is a symlink",
                path.display()
            ));
            continue;
        }
        if !metadata.is_file() {
            warnings.push(format!(
                "skipped {}: journal.json is not a regular file",
                path.display()
            ));
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                warnings.push(format!("skipped {}: read failed: {error}", path.display()));
                continue;
            }
        };
        let journal: Journal = match serde_json::from_slice(&bytes) {
            Ok(journal) => journal,
            Err(error) => {
                warnings.push(format!("skipped {}: parse failed: {error}", path.display()));
                continue;
            }
        };
        journals.push(LinuxUndoJournalSummary {
            journal_path: path.display().to_string(),
            operation: journal.operation,
            entries: journal.entries.len(),
            paths: journal
                .entries
                .into_iter()
                .map(|entry| entry.path)
                .collect(),
        });
    }
    journals.sort_by(|left, right| right.journal_path.cmp(&left.journal_path));
    warnings.sort();
    Ok(LinuxUndoJournalList { journals, warnings })
}

fn execute_operation(
    operation: &str,
    paths: &[PathBuf],
    content: Option<&[u8]>,
) -> Result<(), LinuxApplyError> {
    match operation {
        "write_file" => {
            let path = &paths[0];
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| LinuxApplyError::new(format!("create parent: {error}")))?;
            }
            let content = content.ok_or_else(|| LinuxApplyError::new("missing write content"))?;
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .map_err(|error| LinuxApplyError::new(format!("write file: {error}")))?;
            file.write_all(content)
                .map_err(|error| LinuxApplyError::new(format!("write file: {error}")))?;
        }
        "delete_path" => {
            if paths[0].exists() {
                fs::remove_file(&paths[0])
                    .map_err(|error| LinuxApplyError::new(format!("delete file: {error}")))?;
            }
        }
        "move_path" => {
            if let Some(parent) = paths[1].parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    LinuxApplyError::new(format!("create destination parent: {error}"))
                })?;
            }
            fs::rename(&paths[0], &paths[1])
                .map_err(|error| LinuxApplyError::new(format!("move file: {error}")))?;
        }
        _ => return Err(LinuxApplyError::new("unsupported linux_apply operation")),
    }
    Ok(())
}

fn rollback(snapshots: &[Snapshot]) -> Result<(), LinuxApplyError> {
    for snapshot in snapshots.iter().rev() {
        match &snapshot.content {
            Some(content) => {
                if let Some(parent) = snapshot.path.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        LinuxApplyError::new(format!("rollback parent: {error}"))
                    })?;
                }
                fs::write(&snapshot.path, content)
                    .map_err(|error| LinuxApplyError::new(format!("rollback file: {error}")))?;
            }
            None => {
                if snapshot.path.exists() {
                    fs::remove_file(&snapshot.path).map_err(|error| {
                        LinuxApplyError::new(format!("rollback remove: {error}"))
                    })?;
                }
            }
        }
    }
    Ok(())
}

fn snapshot(path: &Path) -> Result<Snapshot, LinuxApplyError> {
    if !path.exists() {
        return Ok(Snapshot {
            path: path.to_path_buf(),
            content: None,
        });
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| LinuxApplyError::new(format!("inspect target: {error}")))?;
    if !metadata.file_type().is_file() {
        return Err(LinuxApplyError::new(
            "linux_apply only supports regular files",
        ));
    }
    let content = fs::read(path)
        .map_err(|error| LinuxApplyError::new(format!("read existing file: {error}")))?;
    if content.len() > MAX_CONTENT_BYTES {
        return Err(LinuxApplyError::new("existing file exceeds 64 MiB"));
    }
    Ok(Snapshot {
        path: path.to_path_buf(),
        content: Some(content),
    })
}

fn secure_path(
    cwd: &Path,
    root: &Path,
    raw: &str,
    restrict_root: bool,
) -> Result<PathBuf, LinuxApplyError> {
    let candidate = lexical_normalize(&if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    });
    if restrict_root && !candidate.starts_with(root) {
        return Err(LinuxApplyError::new(
            "target escapes the configured workspace",
        ));
    }
    let check_root = if restrict_root {
        root
    } else {
        candidate.ancestors().last().unwrap_or(root)
    };
    let relative = candidate.strip_prefix(check_root).unwrap_or(&candidate);
    let mut current = check_root.to_path_buf();
    for component in relative.components() {
        if matches!(
            component,
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            continue;
        }
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LinuxApplyError::new("symlink targets are not allowed"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LinuxApplyError::new(format!("inspect path: {error}")));
            }
        }
    }
    Ok(candidate)
}

fn reject_yunxi_state(root: &Path, path: &Path) -> Result<(), LinuxApplyError> {
    if path.strip_prefix(root.join(".yunxi")).is_ok() {
        return Err(LinuxApplyError::new(
            "linux_apply cannot mutate YunXi state",
        ));
    }
    Ok(())
}

fn validate_path_text(path: &str, label: &str) -> Result<(), LinuxApplyError> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control) {
        return Err(LinuxApplyError::new(format!(
            "{label} must be non-empty, <= 4096 bytes, and contain no control characters"
        )));
    }
    Ok(())
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn journal_token() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| format!("{}-{}", duration.as_secs(), duration.subsec_nanos()))
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn policy(root: &Path) -> ExecutionPolicy {
        ExecutionPolicy {
            approval: yunxi_agent_sandbox::ApprovalRequirement::PreApproved,
            sandbox: SandboxRequirement::WorkspaceWrite,
            network: yunxi_agent_sandbox::NetworkPolicy::Disabled,
            workspace_root: root.to_path_buf(),
        }
    }

    #[test]
    fn write_delete_and_move_create_journal() {
        let root = tempdir().expect("tempdir");
        let result = apply(
            root.path(),
            LinuxApplyInput::WriteFile {
                path: "a.txt".into(),
                content: "hello".into(),
            },
            &policy(root.path()),
        )
        .expect("write");
        assert_eq!(
            fs::read_to_string(root.path().join("a.txt")).expect("read"),
            "hello"
        );
        assert!(Path::new(&result.journal_path).exists());
        apply(
            root.path(),
            LinuxApplyInput::MovePath {
                from: "a.txt".into(),
                to: "b.txt".into(),
            },
            &policy(root.path()),
        )
        .expect("move");
        apply(
            root.path(),
            LinuxApplyInput::DeletePath {
                path: "b.txt".into(),
            },
            &policy(root.path()),
        )
        .expect("delete");
        assert!(!root.path().join("b.txt").exists());
    }

    #[test]
    fn undo_restores_write_move_and_delete() {
        let root = tempdir().expect("tempdir");
        apply(
            root.path(),
            LinuxApplyInput::WriteFile {
                path: "a.txt".into(),
                content: "before".into(),
            },
            &policy(root.path()),
        )
        .expect("initial write");
        let update = apply(
            root.path(),
            LinuxApplyInput::WriteFile {
                path: "a.txt".into(),
                content: "after".into(),
            },
            &policy(root.path()),
        )
        .expect("update");
        assert_eq!(update.change_kind, LinuxApplyChangeKind::Updated);
        undo(Path::new(&update.journal_path)).expect("undo update");
        assert_eq!(
            fs::read_to_string(root.path().join("a.txt")).expect("read"),
            "before"
        );

        let moved = apply(
            root.path(),
            LinuxApplyInput::MovePath {
                from: "a.txt".into(),
                to: "b.txt".into(),
            },
            &policy(root.path()),
        )
        .expect("move");
        undo(Path::new(&moved.journal_path)).expect("undo move");
        assert_eq!(
            fs::read_to_string(root.path().join("a.txt")).expect("restored"),
            "before"
        );
        assert!(!root.path().join("b.txt").exists());

        let deleted = apply(
            root.path(),
            LinuxApplyInput::DeletePath {
                path: "a.txt".into(),
            },
            &policy(root.path()),
        )
        .expect("delete");
        undo(Path::new(&deleted.journal_path)).expect("undo delete");
        assert_eq!(
            fs::read_to_string(root.path().join("a.txt")).expect("restored"),
            "before"
        );
    }

    #[test]
    fn rejects_traversal_and_symlink() {
        let root = tempdir().expect("tempdir");
        assert!(
            apply(
                root.path(),
                LinuxApplyInput::WriteFile {
                    path: "../escape".into(),
                    content: "x".into()
                },
                &policy(root.path())
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path(), root.path().join("link")).expect("symlink");
            assert!(
                apply(
                    root.path(),
                    LinuxApplyInput::WriteFile {
                        path: "link/a".into(),
                        content: "x".into()
                    },
                    &policy(root.path())
                )
                .is_err()
            );
        }
    }

    #[test]
    fn lists_valid_undo_journals() {
        let root = tempdir().expect("tempdir");
        let result = apply(
            root.path(),
            LinuxApplyInput::WriteFile {
                path: "a.txt".into(),
                content: "hello".into(),
            },
            &policy(root.path()),
        )
        .expect("write");
        let journals = list_journals(root.path()).expect("list journals");
        assert_eq!(journals.len(), 1);
        assert_eq!(journals[0].journal_path, result.journal_path);
        assert_eq!(journals[0].operation, "write_file");
        assert_eq!(journals[0].paths, vec!["a.txt"]);
    }

    #[test]
    fn skips_corrupt_undo_journals_and_keeps_valid_entries() {
        let root = tempdir().expect("tempdir");
        let result = apply(
            root.path(),
            LinuxApplyInput::WriteFile {
                path: "a.txt".into(),
                content: "hello".into(),
            },
            &policy(root.path()),
        )
        .expect("write");
        let corrupt_dir = root.path().join(".yunxi/undo/corrupt");
        fs::create_dir_all(&corrupt_dir).expect("corrupt journal directory");
        fs::write(corrupt_dir.join("journal.json"), b"{broken").expect("corrupt journal");

        let list = list_journals_with_warnings(root.path()).expect("list journals");

        assert_eq!(list.journals.len(), 1);
        assert_eq!(list.journals[0].journal_path, result.journal_path);
        assert_eq!(list.warnings.len(), 1);
        assert!(list.warnings[0].contains("corrupt"));
        assert!(list.warnings[0].contains("parse failed"));
    }

    #[cfg(unix)]
    #[test]
    fn skips_inner_journal_symlinks() {
        let root = tempdir().expect("tempdir");
        let valid = apply(
            root.path(),
            LinuxApplyInput::WriteFile {
                path: "a.txt".into(),
                content: "hello".into(),
            },
            &policy(root.path()),
        )
        .expect("write");
        let outside = tempdir().expect("outside tempdir");
        let symlink_dir = root.path().join(".yunxi/undo/symlinked");
        fs::create_dir_all(&symlink_dir).expect("symlink journal directory");
        fs::write(outside.path().join("journal.json"), b"{}").expect("outside journal target");
        std::os::unix::fs::symlink(
            outside.path().join("journal.json"),
            symlink_dir.join("journal.json"),
        )
        .expect("inner journal symlink");

        let list = list_journals_with_warnings(root.path()).expect("list journals");

        assert_eq!(list.journals.len(), 1);
        assert_eq!(list.journals[0].journal_path, valid.journal_path);
        assert_eq!(list.warnings.len(), 1);
        assert!(list.warnings[0].contains("journal.json is a symlink"));
    }
}
