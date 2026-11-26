use serde::{Deserialize, Serialize};

/// The working style a Session runs: which tool surface the model sees. The
/// mode is Session configuration and does not change what the Session mounts;
/// see `docs/design/agent-tool-modes.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolMode {
    /// One tool call per decision: every mounted tool except the two entry
    /// tools is declared.
    #[default]
    Normal,
    /// One program per step: only `codemode` and `skill` are declared, and a
    /// program may call every mounted tool except `codemode`.
    Codemode,
    /// One persistent shell: only `execute`, `ask_user`, `goal_update` and
    /// `recall` are declared.
    Shell,
}

impl ToolMode {
    /// Whether this is `normal`, the only mode a stored record omits.
    pub fn is_normal(&self) -> bool {
        matches!(self, Self::Normal)
    }

    /// The mode's stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Codemode => "codemode",
            Self::Shell => "shell",
        }
    }

    /// The tool this mode is entered through, if any. A mode whose entry tool
    /// is not mounted runs `normal`.
    pub fn entry_tool(self) -> Option<&'static str> {
        match self {
            Self::Normal => None,
            Self::Codemode => Some("codemode"),
            Self::Shell => Some("execute"),
        }
    }
}
