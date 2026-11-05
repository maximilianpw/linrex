mod model;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub use model::*;

const ENDPOINT: &str = "https://api.linear.app/graphql";
const PAGE_LIMIT: usize = 5;

const ISSUE_FIELDS: &str = "
fragment IssueFields on Issue {
  id identifier title description url branchName priority updatedAt
  state { id name type color position }
  team { id key name }
  assignee { id displayName }
  project { name }
  labels { nodes { name } }
}";

#[derive(Debug, thiserror::Error)]
pub enum LinearError {
    #[error("Linear rejected the API key (HTTP {0})")]
    Unauthorized(u16),
    #[error("Linear returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("could not reach Linear: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Linear: {0}")]
    Graphql(String),
    #[error("unexpected Linear response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("Linear did not apply the {0}")]
    NotApplied(&'static str),
}

pub type LinearResult<T> = Result<T, LinearError>;

#[derive(Clone)]
pub struct LinearClient {
    http: reqwest::blocking::Client,
    api_key: String,
    endpoint: String,
}

impl std::fmt::Debug for LinearClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearClient").finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssuePage {
    nodes: Vec<model::RawIssue>,
    page_info: PageInfo,
}

impl LinearClient {
    pub fn new(api_key: String) -> LinearResult<Self> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("linrex/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let endpoint =
            std::env::var("LINREX_LINEAR_ENDPOINT").unwrap_or_else(|_| ENDPOINT.to_owned());
        Ok(Self {
            http,
            api_key,
            endpoint,
        })
    }

    fn request<T: DeserializeOwned>(&self, query: &str, variables: Value) -> LinearResult<T> {
        let response = self
            .http
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .json(&json!({ "query": query, "variables": variables }))
            .send()?;
        let status = response.status();
        let body = response.text()?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(LinearError::Unauthorized(status.as_u16()));
        }
        decode_response(status.as_u16(), &body)
    }

    pub fn snapshot(&self) -> LinearResult<WorkspaceSnapshot> {
        #[derive(Deserialize)]
        struct Data {
            viewer: Viewer,
            organization: model::Named,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Viewer {
            id: String,
            display_name: String,
            teams: model::Nodes<Team>,
        }
        let data: Data = self.request(
            "query { viewer { id displayName teams(first: 100) { nodes { id key name } } } organization { name } }",
            json!({}),
        )?;
        let active = json!({ "state": { "type": { "nin": ["completed", "canceled"] } } });

        let issues = self.issue_pages(
            "/viewer/assignedIssues",
            &format!(
                "query($after: String, $filter: IssueFilter) {{ viewer {{ assignedIssues(first: 100, after: $after, filter: $filter, orderBy: updatedAt) {{ nodes {{ ...IssueFields }} pageInfo {{ hasNextPage endCursor }} }} }} }} {ISSUE_FIELDS}"
            ),
            active,
        )?;

        Ok(WorkspaceSnapshot {
            viewer: User {
                id: data.viewer.id,
                display_name: data.viewer.display_name,
            },
            organization: data.organization.name,
            teams: data.viewer.teams.nodes,
            issues,
        })
    }

    fn issue_pages(&self, page_path: &str, query: &str, filter: Value) -> LinearResult<Vec<Issue>> {
        let mut issues = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..PAGE_LIMIT {
            let data: Value = self.request(query, json!({ "after": after, "filter": filter }))?;
            let page = data
                .pointer(page_path)
                .ok_or_else(|| LinearError::Graphql(format!("response is missing {page_path}")))?;
            let page = IssuePage::deserialize(page)?;
            issues.extend(page.nodes.into_iter().map(Issue::from));
            match page.page_info {
                PageInfo {
                    has_next_page: true,
                    end_cursor: Some(cursor),
                } => after = Some(cursor),
                _ => break,
            }
        }
        Ok(issues)
    }

    pub fn issue(&self, id: &str) -> LinearResult<Issue> {
        #[derive(Deserialize)]
        struct Data {
            issue: model::RawIssue,
        }
        let data: Data = self.request(
            &format!(
                "query($id: String!) {{ issue(id: $id) {{ ...IssueFields }} }} {ISSUE_FIELDS}"
            ),
            json!({ "id": id }),
        )?;
        Ok(data.issue.into())
    }

    pub fn comments(&self, issue_id: &str) -> LinearResult<Vec<Comment>> {
        #[derive(Deserialize)]
        struct Data {
            issue: IssueComments,
        }
        #[derive(Deserialize)]
        struct IssueComments {
            comments: model::Nodes<Comment>,
        }
        let data: Data = self.request(
            "query($id: String!) { issue(id: $id) { comments(first: 50) { nodes { body createdAt user { id displayName } } } } }",
            json!({ "id": issue_id }),
        )?;
        let mut comments = data.issue.comments.nodes;
        comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(comments)
    }

    pub fn team_meta(&self, team_id: &str) -> LinearResult<TeamMeta> {
        #[derive(Deserialize)]
        struct Data {
            team: TeamData,
        }
        #[derive(Deserialize)]
        struct TeamData {
            states: model::Nodes<WorkflowState>,
            members: model::Nodes<User>,
        }
        let data: Data = self.request(
            "query($id: String!) { team(id: $id) { states { nodes { id name type color position } } members(first: 250) { nodes { id displayName } } } }",
            json!({ "id": team_id }),
        )?;
        let mut states = data.team.states.nodes;
        states.sort_by(|a, b| {
            state_order(a.kind)
                .cmp(&state_order(b.kind))
                .then(a.position.total_cmp(&b.position))
        });
        let mut members = data.team.members.nodes;
        members.sort_by_key(|m| m.display_name.to_lowercase());
        Ok(TeamMeta { states, members })
    }

    pub fn update_issue(&self, issue_id: &str, update: &IssueUpdate) -> LinearResult<Issue> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            issue_update: Payload,
        }
        let mut input = serde_json::Map::new();
        if let Some(state_id) = &update.state_id {
            input.insert("stateId".into(), json!(state_id));
        }
        if let Some(assignee) = &update.assignee_id {
            input.insert("assigneeId".into(), json!(assignee));
        }
        if let Some(priority) = update.priority {
            input.insert("priority".into(), json!(priority.value()));
        }
        let data: Data = self.request(
            &format!(
                "mutation($id: String!, $input: IssueUpdateInput!) {{ issueUpdate(id: $id, input: $input) {{ success issue {{ ...IssueFields }} }} }} {ISSUE_FIELDS}"
            ),
            json!({ "id": issue_id, "input": input }),
        )?;
        data.issue_update.into_issue("issue update")
    }

    pub fn create_issue(&self, new: &NewIssue) -> LinearResult<Issue> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            issue_create: Payload,
        }
        let data: Data = self.request(
            &format!(
                "mutation($input: IssueCreateInput!) {{ issueCreate(input: $input) {{ success issue {{ ...IssueFields }} }} }} {ISSUE_FIELDS}"
            ),
            json!({ "input": {
                "teamId": new.team_id,
                "title": new.title,
                "description": new.description,
                "assigneeId": new.assignee_id,
            } }),
        )?;
        data.issue_create.into_issue("issue creation")
    }

    pub fn add_comment(&self, issue_id: &str, body: &str) -> LinearResult<()> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            comment_create: Success,
        }
        #[derive(Deserialize)]
        struct Success {
            success: bool,
        }
        let data: Data = self.request(
            "mutation($input: CommentCreateInput!) { commentCreate(input: $input) { success } }",
            json!({ "input": { "issueId": issue_id, "body": body } }),
        )?;
        if data.comment_create.success {
            Ok(())
        } else {
            Err(LinearError::NotApplied("comment"))
        }
    }
}

#[derive(Deserialize)]
struct Payload {
    success: bool,
    issue: Option<model::RawIssue>,
}

impl Payload {
    fn into_issue(self, what: &'static str) -> LinearResult<Issue> {
        match self {
            Self {
                success: true,
                issue: Some(issue),
            } => Ok(issue.into()),
            _ => Err(LinearError::NotApplied(what)),
        }
    }
}

fn state_order(kind: StateType) -> u8 {
    match kind {
        StateType::Triage => 0,
        StateType::Backlog => 1,
        StateType::Unstarted => 2,
        StateType::Started => 3,
        StateType::Completed => 4,
        StateType::Canceled => 5,
        StateType::Other => 6,
    }
}

fn decode_response<T: DeserializeOwned>(status: u16, body: &str) -> LinearResult<T> {
    let parsed: GraphqlResponse<T> = match serde_json::from_str(body) {
        Ok(parsed) => parsed,
        Err(_) if !(200..300).contains(&status) => {
            return Err(LinearError::Http {
                status,
                body: body.chars().take(300).collect(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !parsed.errors.is_empty() {
        let messages: Vec<String> = parsed.errors.into_iter().map(|e| e.message).collect();
        return Err(LinearError::Graphql(messages.join("; ")));
    }
    parsed
        .data
        .ok_or_else(|| LinearError::Graphql("response had no data".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_issue_and_normalizes_fields() {
        #[derive(Deserialize)]
        struct Data {
            issue: model::RawIssue,
        }
        let body = r##"{"data":{"issue":{
            "id":"i1","identifier":"ENG-1129","title":"Flag","description":"  ",
            "url":"https://linear.app/acme/issue/ENG-1129","branchName":"sam/eng-1129-flag",
            "priority":2,"updatedAt":"2026-11-01T10:00:00.000Z",
            "state":{"id":"s1","name":"In Progress","type":"started","color":"#f2c94c","position":2},
            "team":{"id":"t1","key":"ENG","name":"ENG"},
            "assignee":null,"project":{"name":"Payments"},
            "labels":{"nodes":[{"name":"Bug"}]}}}}"##;
        let data: Data = decode_response(200, body).unwrap();
        let issue = Issue::from(data.issue);
        assert_eq!(issue.priority, Priority::High);
        assert_eq!(issue.state.kind, StateType::Started);
        assert_eq!(issue.description, None);
        assert_eq!(issue.project.as_deref(), Some("Payments"));
        assert_eq!(issue.labels, ["Bug"]);
    }

    #[test]
    fn surfaces_graphql_errors() {
        let error = decode_response::<Value>(
            200,
            r#"{"data":null,"errors":[{"message":"Entity not found"}]}"#,
        )
        .unwrap_err();
        assert!(matches!(error, LinearError::Graphql(m) if m == "Entity not found"));
    }

    #[test]
    fn reports_non_json_http_failures() {
        let error = decode_response::<Value>(502, "Bad gateway").unwrap_err();
        assert!(matches!(error, LinearError::Http { status: 502, .. }));
    }

    #[test]
    fn unknown_state_types_do_not_fail_decoding() {
        let state: WorkflowState =
            serde_json::from_str(r##"{"id":"s","name":"Dup","type":"duplicate","color":"#000"}"##)
                .unwrap();
        assert_eq!(state.kind, StateType::Other);
    }
}
