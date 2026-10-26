use serde::Deserialize;

/// How safe a folder is to remove. Ordered by urgency, not alphabetically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Tier {
    SafePurge,
    Review,
    Keep,
    Locked,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::SafePurge => "SAFE_PURGE",
            Tier::Review => "REVIEW",
            Tier::Keep => "KEEP",
            Tier::Locked => "LOCKED",
        }
    }

    /// `Locked` and `Keep` must never be offered for removal.
    pub fn is_protective(self) -> bool {
        matches!(self, Tier::Keep | Tier::Locked)
    }
}

/// Matchers on one rule are ANDed: every field that is present must pass.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuleMatch {
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub regex: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: String,
    pub tier: Tier,
    pub category: String,
    pub reason: String,
    #[serde(default)]
    pub impact: Option<String>,
    /// Colours a map, protects nothing, never becomes a candidate.
    #[serde(default)]
    pub display: bool,
    #[serde(default)]
    pub min_bytes: Option<u64>,
    /// Currently only `"package.json"`: match only when the PARENT contains it.
    #[serde(default)]
    pub requires_sibling: Option<String>,
    #[serde(rename = "match")]
    pub matcher: RuleMatch,
}

/// One scanned directory. `size` is recursive; `has_package_json` describes the
/// directory itself, not its children.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: String,
    pub size: u64,
    pub files: u64,
    pub mtime_ms: i64,
    pub has_package_json: bool,
}

impl DirEntry {
    pub fn new(path: impl Into<String>, size: u64) -> Self {
        Self {
            path: path.into(),
            size,
            files: 0,
            mtime_ms: 0,
            has_package_json: false,
        }
    }

    pub fn with_package_json(mut self, value: bool) -> Self {
        self.has_package_json = value;
        self
    }
}
