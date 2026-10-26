use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};

const DIFF_PREVIEW_LIMIT: usize = 12_000;

#[derive(Clone, Debug, Default)]
pub struct WorkspaceStatus {
    pub root: PathBuf,
    pub is_git_repo: bool,
    pub branch: Option<String>,
    pub files: Vec<ChangedFile>,
    pub staged_diff_preview: String,
    pub unstaged_diff_preview: String,
    pub diff_preview: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub status: String,
    pub path: String,
    pub old_path: Option<String>,
    pub staged: bool,
    pub unstaged: bool,
}

impl WorkspaceStatus {
    pub fn empty(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ..Self::default()
        }
    }

    /// Read-only Git inspection. Callers running on an async executor should
    /// invoke this through `spawn_blocking` or the workspace refresh worker.
    pub fn read(root: &Path) -> Result<Self> {
        let root = root.to_path_buf();
        let git_root_output = match Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "--show-toplevel"])
            .output()
        {
            Ok(output) if output.status.success() => output,
            Ok(_) => return Ok(Self::empty(root)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    root,
                    error: Some("Git is not available".to_string()),
                    ..Self::default()
                });
            }
            Err(error) => return Err(error.into()),
        };

        let repository_root = PathBuf::from(
            String::from_utf8_lossy(&git_root_output.stdout)
                .trim()
                .to_string(),
        );
        let branch = git_text(&repository_root, &["branch", "--show-current"])?
            .trim()
            .to_string();
        let status_output = git_command(
            &repository_root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        let files = parse_status(&status_output.stdout);
        let staged_diff_preview = git_text(
            &repository_root,
            &["diff", "--cached", "--no-ext-diff", "--unified=2", "--"],
        )?;
        let unstaged_diff_preview = git_text(
            &repository_root,
            &["diff", "--no-ext-diff", "--unified=2", "--"],
        )?;
        let diff_preview = combine_diffs(&staged_diff_preview, &unstaged_diff_preview);

        Ok(Self {
            root: repository_root,
            is_git_repo: true,
            branch: (!branch.is_empty()).then_some(branch),
            files,
            staged_diff_preview,
            unstaged_diff_preview,
            diff_preview,
            error: None,
        })
    }
}

fn git_command(root: &Path, args: &[&str]) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("git {} failed: {}", args.join(" "), stderr);
    }
    Ok(output)
}

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_command(root, args)?.stdout).to_string())
}

fn parse_status(bytes: &[u8]) -> Vec<ChangedFile> {
    let mut records = Vec::new();
    let mut fields = bytes.split(|byte| *byte == 0);
    while let Some(raw) = fields.next() {
        if raw.len() < 4 {
            continue;
        }
        let index_status = raw[0] as char;
        let worktree_status = raw[1] as char;
        let path = String::from_utf8_lossy(&raw[3..]).into_owned();
        let old_path = if matches!(index_status, 'R' | 'C') {
            fields
                .next()
                .filter(|value| !value.is_empty())
                .map(|value| String::from_utf8_lossy(value).into_owned())
        } else {
            None
        };
        records.push(ChangedFile {
            status: format!("{index_status}{worktree_status}"),
            path,
            old_path,
            staged: index_status != ' ' && index_status != '?',
            unstaged: worktree_status != ' ' || index_status == '?',
        });
    }
    records
}

fn combine_diffs(staged: &str, unstaged: &str) -> String {
    let mut combined = String::new();
    if !staged.trim().is_empty() {
        combined.push_str("[staged]\n");
        combined.push_str(staged);
    }
    if !unstaged.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("[unstaged]\n");
        combined.push_str(unstaged);
    }
    truncate_preview(combined)
}

fn truncate_preview(mut value: String) -> String {
    if value.len() <= DIFF_PREVIEW_LIMIT {
        return value;
    }
    let mut end = DIFF_PREVIEW_LIMIT;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("\n… diff preview truncated …");
    value
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[test]
    fn reads_staged_and_unstaged_changes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        run_git(root, &["init", "-q"])?;
        fs::write(root.join("tracked.txt"), "one\n")?;
        run_git(root, &["add", "tracked.txt"])?;
        fs::write(root.join("tracked.txt"), "one\ntwo\n")?;

        let status = WorkspaceStatus::read(root)?;
        assert!(status.is_git_repo);
        assert_eq!(status.files.len(), 1);
        assert_eq!(status.files[0].status, "AM");
        assert!(status.files[0].staged);
        assert!(status.files[0].unstaged);
        assert!(status.staged_diff_preview.contains("+one"));
        assert!(status.unstaged_diff_preview.contains("+two"));
        assert!(status.diff_preview.contains("[staged]"));
        assert!(status.diff_preview.contains("[unstaged]"));
        Ok(())
    }

    #[test]
    fn a_non_git_directory_is_a_valid_empty_workspace() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let status = WorkspaceStatus::read(directory.path())?;
        assert!(!status.is_git_repo);
        assert!(status.files.is_empty());
        assert!(status.diff_preview.is_empty());
        Ok(())
    }

    fn run_git(root: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()?;
        anyhow::ensure!(output.status.success(), "git command failed");
        Ok(())
    }
}
