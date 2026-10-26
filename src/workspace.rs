use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;

#[derive(Clone, Debug, Default)]
pub struct WorkspaceStatus {
    pub root: PathBuf,
    pub is_git_repo: bool,
    pub branch: Option<String>,
    pub files: Vec<ChangedFile>,
    pub diff_preview: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ChangedFile {
    pub status: String,
    pub path: String,
}

impl WorkspaceStatus {
    pub fn read(root: &Path) -> Result<Self> {
        let root = root.to_path_buf();
        let git_root = Command::new("git")
            .args([
                "-C",
                root.to_string_lossy().as_ref(),
                "rev-parse",
                "--show-toplevel",
            ])
            .output();

        let Ok(git_root) = git_root else {
            return Ok(Self {
                root,
                error: Some("Git is not available".to_string()),
                ..Self::default()
            });
        };
        if !git_root.status.success() {
            return Ok(Self {
                root,
                ..Self::default()
            });
        }

        let branch = git_output(&root, &["branch", "--show-current"])?;
        let status = git_output(&root, &["status", "--short"])?;
        let files = status
            .lines()
            .filter_map(|line| {
                let (status, path) = line.split_at(line.len().min(3));
                let path = path.trim();
                (!path.is_empty()).then(|| ChangedFile {
                    status: status.trim().to_string(),
                    path: path.to_string(),
                })
            })
            .collect();
        let mut diff_preview = git_output(&root, &["diff", "--no-ext-diff", "--unified=2", "--"])?;
        if diff_preview.len() > 12_000 {
            diff_preview.truncate(12_000);
            diff_preview.push_str("\n… diff preview truncated …");
        }

        Ok(Self {
            root,
            is_git_repo: true,
            branch: (!branch.is_empty()).then_some(branch),
            files,
            diff_preview,
            error: None,
        })
    }
}

fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
