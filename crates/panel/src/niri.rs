//! Talking to niri over its CLI (`niri msg`).
//!
//! Querying and acting are both plain subprocess calls. They are synchronous
//! and cheap (a few milliseconds), so the panel simply runs them inline.

use std::process::Command;

/// One workspace as reported by `niri msg --json workspaces`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Workspace {
    pub id: u64,
    pub idx: u32,
    #[serde(default)]
    pub output: String,
    #[serde(rename = "is_focused", default)]
    pub is_focused: bool,
    /// Filled in by [`workspaces`] from the window list; niri's workspace
    /// objects don't include their windows directly.
    #[serde(skip)]
    pub has_windows: bool,
}

impl Workspace {
    /// A workspace is "occupied" once it holds at least one window.
    pub fn is_occupied(&self) -> bool {
        self.has_windows
    }
}

/// A window as reported by `niri msg --json windows` (only the field we need).
#[derive(Debug, serde::Deserialize)]
struct Window {
    workspace_id: Option<u64>,
}

/// The set of workspace ids that currently hold at least one window.
fn occupied_workspaces() -> Result<Vec<u64>, String> {
    let out = Command::new("niri")
        .args(["msg", "--json", "windows"])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }

    let windows: Vec<Window> = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;

    Ok(windows.into_iter().filter_map(|w| w.workspace_id).collect())
}

/// All workspaces, sorted by output then index — the order the pager draws
/// them in.
pub fn workspaces() -> Result<Vec<Workspace>, String> {
    let out = Command::new("niri")
        .args(["msg", "--json", "workspaces"])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }

    let mut list: Vec<Workspace> =
        serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;

    let occupied = occupied_workspaces().unwrap_or_default();

    for workspace in &mut list {
        workspace.has_windows = occupied.contains(&workspace.id);
    }

    list.sort_by(|a, b| a.output.cmp(&b.output).then(a.idx.cmp(&b.idx)));
    Ok(list)
}

/// Focus the workspace with the given index.
pub fn focus_workspace(idx: u32) {
    let _ = Command::new("niri")
        .args(["msg", "action", "focus-workspace", &idx.to_string()])
        .spawn();
}
