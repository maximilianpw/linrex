use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateType {
    Triage,
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
    #[serde(other)]
    Other,
}

impl StateType {
    pub fn is_not_started(self) -> bool {
        matches!(self, Self::Triage | Self::Backlog | Self::Unstarted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    None,
    Urgent,
    High,
    Medium,
    Low,
}

impl Priority {
    pub const ALL: [Self; 5] = [
        Self::Urgent,
        Self::High,
        Self::Medium,
        Self::Low,
        Self::None,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No priority",
            Self::Urgent => "Urgent",
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
        }
    }

    pub fn value(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Urgent => 1,
            Self::High => 2,
            Self::Medium => 3,
            Self::Low => 4,
        }
    }

    pub fn sort_rank(self) -> u8 {
        match self {
            Self::None => 5,
            other => other.value(),
        }
    }
}

impl<'de> Deserialize<'de> for Priority {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        Ok(match value as u8 {
            1 => Self::Urgent,
            2 => Self::High,
            3 => Self::Medium,
            4 => Self::Low,
            _ => Self::None,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: StateType,
    pub color: String,
    #[serde(default)]
    pub position: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Team {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Named {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Nodes<T> {
    pub nodes: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawIssue {
    id: String,
    identifier: String,
    title: String,
    description: Option<String>,
    url: String,
    branch_name: String,
    priority: Priority,
    updated_at: String,
    state: WorkflowState,
    team: Team,
    assignee: Option<User>,
    project: Option<Named>,
    labels: Nodes<Named>,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub url: String,
    pub branch_name: String,
    pub priority: Priority,
    pub updated_at: String,
    pub state: WorkflowState,
    pub team: Team,
    pub assignee: Option<User>,
    pub project: Option<String>,
    pub labels: Vec<String>,
}

impl From<RawIssue> for Issue {
    fn from(raw: RawIssue) -> Self {
        Self {
            id: raw.id,
            identifier: raw.identifier,
            title: raw.title,
            description: raw.description.filter(|d| !d.trim().is_empty()),
            url: raw.url,
            branch_name: raw.branch_name,
            priority: raw.priority,
            updated_at: raw.updated_at,
            state: raw.state,
            team: raw.team,
            assignee: raw.assignee,
            project: raw.project.map(|p| p.name),
            labels: raw.labels.nodes.into_iter().map(|l| l.name).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub body: String,
    pub created_at: String,
    pub user: Option<User>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceSnapshot {
    pub viewer: User,
    pub organization: String,
    pub teams: Vec<Team>,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone)]
pub struct TeamMeta {
    pub states: Vec<WorkflowState>,
    pub members: Vec<User>,
}

impl TeamMeta {
    pub fn first_started_state(&self) -> Option<&WorkflowState> {
        self.states
            .iter()
            .filter(|s| s.kind == StateType::Started)
            .min_by(|a, b| a.position.total_cmp(&b.position))
    }
}

#[derive(Debug, Default, Clone)]
pub struct IssueUpdate {
    pub state_id: Option<String>,
    pub assignee_id: Option<Option<String>>,
    pub priority: Option<Priority>,
}

#[derive(Debug, Clone)]
pub struct NewIssue {
    pub team_id: String,
    pub title: String,
    pub description: Option<String>,
    pub assignee_id: Option<String>,
}
