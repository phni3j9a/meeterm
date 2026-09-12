//! Backend-independent, low-frequency control-plane metadata.
//!
//! IDs are opaque to the UI and are resolved only inside the owning SSH
//! connection/runtime. Native terminal handles have a separate lifetime.
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Tmux,
    Herdr,
}

impl Backend {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tmux" => Some(Self::Tmux),
            "herdr" => Some(Self::Herdr),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub backend: Backend,
    pub runtime: String,
    pub groups_supported: bool,
    pub workspaces: Vec<Workspace>,
    pub groups: Vec<TerminalGroup>,
    pub terminals: Vec<Terminal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalGroup {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminal {
    pub id: String,
    pub workspace_id: String,
    pub group_id: String,
    pub terminal_id: String,
    pub name: String,
    pub active: bool,
    pub selected: bool,
    pub agent: Option<Agent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Agent {
    pub name: String,
    pub status: String,
}

impl RuntimeSnapshot {
    pub(crate) fn tmux(snapshot: &crate::tmux::SessionSnapshot) -> Self {
        Self {
            backend: Backend::Tmux,
            runtime: crate::tmux::SESSION_NAME.to_owned(),
            groups_supported: false,
            workspaces: snapshot
                .windows
                .iter()
                .map(|window| Workspace {
                    id: format!("@{}", window.window_id),
                    name: window.name.clone(),
                })
                .collect(),
            // A virtual group never creates or rearranges anything in tmux.
            groups: snapshot
                .windows
                .iter()
                .map(|window| TerminalGroup {
                    id: format!("@{}", window.window_id),
                    workspace_id: format!("@{}", window.window_id),
                    name: String::new(),
                    selected: window.selected,
                })
                .collect(),
            terminals: snapshot
                .panes
                .iter()
                .map(|pane| Terminal {
                    id: format!("%{}", pane.pane_id),
                    workspace_id: format!("@{}", pane.window_id),
                    group_id: format!("@{}", pane.window_id),
                    terminal_id: format!("native:{}", pane.terminal_id),
                    name: pane.pane_name.clone(),
                    active: pane.active,
                    selected: pane.selected,
                    agent: None,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{PaneSnapshot, SessionSnapshot, WindowSnapshot};

    #[test]
    fn tmux_projection_keeps_remote_layout_and_native_identity_distinct() {
        let pane = PaneSnapshot {
            window_id: 7,
            pane_id: 12,
            terminal_id: 82,
            window_name: "work".into(),
            pane_name: "shell".into(),
            title: "shell".into(),
            active: true,
            selected: true,
            index: 0,
            columns: 40,
            rows: 20,
        };
        let source = SessionSnapshot {
            selected_pane: Some(12),
            panes: vec![pane.clone()],
            windows: vec![WindowSnapshot {
                window_id: 7,
                name: "work".into(),
                panes: vec![pane],
                selected: true,
                zoomed: false,
            }],
        };
        let original = source.clone();
        let result = RuntimeSnapshot::tmux(&source);
        assert_eq!(source, original);
        assert_eq!(result.runtime, "meeterm");
        assert!(!result.groups_supported);
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].workspace_id, result.workspaces[0].id);
        assert_eq!(result.terminals[0].group_id, result.groups[0].id);
        assert_eq!(result.terminals[0].id, "%12");
        assert_eq!(result.terminals[0].terminal_id, "native:82");
        assert!(result.terminals[0].agent.is_none());
    }
}
