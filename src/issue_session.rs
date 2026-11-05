use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::checkout::{Checkout, CheckoutRequest, Repo, Vcs, ensure_checkout};
use crate::linear::{Comment, Issue, IssueUpdate, LinearClient};
use crate::rex::{NewSession, Rex, RexSession};
use crate::workspace_command;

pub fn session_prefix(identifier: &str) -> String {
    identifier.to_ascii_lowercase()
}

pub fn session_label(issue: &Issue) -> String {
    let mut label = session_prefix(&issue.identifier);
    let words = issue
        .title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(4);
    for word in words {
        if label.len() + word.len() + 1 > 48 {
            break;
        }
        label.push('-');
        label.push_str(&word.to_ascii_lowercase());
    }
    label
}

pub fn find_session<'a>(sessions: &'a [RexSession], identifier: &str) -> Option<&'a RexSession> {
    let prefix = session_prefix(identifier);
    sessions.iter().find(|s| {
        s.label == prefix
            || s.label
                .strip_prefix(&prefix)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

pub fn issue_prompt(issue: &Issue, comments: &[Comment], notes: Option<&str>) -> String {
    let mut prompt = format!(
        "Work on Linear issue {}: {}\n\nURL: {}\nTeam: {} · Status: {} · Priority: {}\n",
        issue.identifier,
        issue.title,
        issue.url,
        issue.team.name,
        issue.state.name,
        issue.priority.label()
    );
    if let Some(project) = &issue.project {
        let _ = writeln!(prompt, "Project: {project}");
    }
    if !issue.labels.is_empty() {
        let _ = writeln!(prompt, "Labels: {}", issue.labels.join(", "));
    }
    let _ = writeln!(
        prompt,
        "Branch or bookmark name Linear links to this issue: {}",
        issue.branch_name
    );
    let _ = write!(
        prompt,
        "\n## Description\n\n{}\n",
        issue.description.as_deref().unwrap_or("(no description)")
    );
    if !comments.is_empty() {
        prompt.push_str("\n## Comments\n");
        for comment in comments.iter().rev().take(15).rev() {
            let author = comment
                .user
                .as_ref()
                .map_or("unknown", |u| u.display_name.as_str());
            let _ = write!(
                prompt,
                "\n**{author}** ({}):\n{}\n",
                comment.created_at, comment.body
            );
        }
    }
    if let Some(notes) = notes.map(str::trim).filter(|n| !n.is_empty()) {
        let _ = write!(prompt, "\n## Notes from me\n\n{notes}\n");
    }
    prompt
}

#[derive(Debug)]
pub enum Opened {
    Existing {
        session_id: String,
        label: String,
    },
    Created {
        session_id: String,
        label: String,
        checkout: PathBuf,
        checkout_created: bool,
        status: StatusChange,
    },
}

#[derive(Debug)]
pub enum StatusChange {
    Unchanged,
    Moved(String),
    Failed(String),
}

pub struct StartOptions<'a> {
    pub repo: &'a Repo,
    pub per_issue_checkout: bool,
    pub command: &'a [String],
    pub notes: Option<&'a str>,
    pub move_to_started: bool,
}

impl Opened {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Existing { session_id, .. } | Self::Created { session_id, .. } => session_id,
        }
    }
}

pub fn start(
    rex: &Rex,
    linear: &LinearClient,
    issue: &Issue,
    options: &StartOptions<'_>,
) -> Result<Opened> {
    if let Some(session) = find_session(&rex.sessions()?, &issue.identifier) {
        return Ok(Opened::Existing {
            session_id: session.session_id.clone(),
            label: session.label.clone(),
        });
    }

    let checkout = if options.per_issue_checkout {
        let description = format!("{}: {}", issue.identifier, issue.title);
        ensure_checkout(
            options.repo,
            &CheckoutRequest {
                issue: &session_prefix(&issue.identifier),
                branch: &issue.branch_name,
                description: &description,
            },
        )?
    } else {
        Checkout {
            path: options.repo.path.clone(),
            vcs: options.repo.vcs(),
            created: false,
        }
    };

    let comments = linear
        .comments(&issue.id)
        .context("could not load the issue's comments")?;
    let mut command = options.command.to_vec();
    command.push(issue_prompt(issue, &comments, options.notes));
    let (cwd, command) = if checkout.vcs == Vcs::Command && options.per_issue_checkout {
        let create = workspace_command::expand(
            &options.repo.create,
            &workspace_command::Values {
                identifier: &issue.identifier,
                branch: &issue.branch_name,
                title: &issue.title,
                dir: &checkout.path,
            },
        );
        let command = workspace_command::session_command(&create, &checkout.path, &command);
        (options.repo.path.as_path(), command)
    } else {
        (checkout.path.as_path(), command)
    };

    let label = session_label(issue);
    let session_id = rex.create_session(&NewSession {
        label: &label,
        cwd,
        window: &issue.identifier,
        command: &command,
    })?;

    let status = if options.move_to_started && issue.state.kind.is_not_started() {
        match move_to_started(linear, issue) {
            Ok(Some(state)) => StatusChange::Moved(state),
            Ok(None) => StatusChange::Unchanged,
            Err(error) => StatusChange::Failed(format!("{error:#}")),
        }
    } else {
        StatusChange::Unchanged
    };

    Ok(Opened::Created {
        session_id,
        label,
        checkout: checkout.path,
        checkout_created: checkout.created,
        status,
    })
}

fn move_to_started(linear: &LinearClient, issue: &Issue) -> Result<Option<String>> {
    let meta = linear.team_meta(&issue.team.id)?;
    let Some(state) = meta.first_started_state() else {
        return Ok(None);
    };
    linear.update_issue(
        &issue.id,
        &IssueUpdate {
            state_id: Some(state.id.clone()),
            ..IssueUpdate::default()
        },
    )?;
    Ok(Some(state.name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::{Priority, StateType, Team, WorkflowState};

    fn issue(identifier: &str, title: &str) -> Issue {
        Issue {
            id: "id".into(),
            identifier: identifier.into(),
            title: title.into(),
            description: Some("Do the thing".into()),
            url: "https://linear.app/x".into(),
            branch_name: "sam/eng-1129-flag".into(),
            priority: Priority::High,
            updated_at: String::new(),
            state: WorkflowState {
                id: "s".into(),
                name: "Todo".into(),
                kind: StateType::Unstarted,
                color: String::new(),
                position: 0.0,
            },
            team: Team {
                id: "t".into(),
                key: "ENG".into(),
                name: "ENG".into(),
            },
            assignee: None,
            project: None,
            labels: vec![],
        }
    }

    fn session(label: &str) -> RexSession {
        RexSession {
            session_id: format!("session:{label}"),
            label: label.into(),
        }
    }

    #[test]
    fn labels_follow_existing_issue_session_convention() {
        assert_eq!(
            session_label(&issue("ENG-1129", "Login redirect: loop (v2) on Safari!")),
            "eng-1129-login-redirect-loop-v2"
        );
    }

    #[test]
    fn finds_session_by_identifier_prefix_only_at_word_boundary() {
        let sessions = [
            session("eng-1129-login-redirect-loop"),
            session("eng-11"),
            session("eng-112x"),
        ];
        assert_eq!(
            find_session(&sessions, "ENG-1129").unwrap().label,
            "eng-1129-login-redirect-loop"
        );
        assert_eq!(find_session(&sessions, "ENG-11").unwrap().label, "eng-11");
        assert!(find_session(&sessions, "ENG-112").is_none());
    }

    #[test]
    fn prompt_includes_description_and_latest_comments() {
        let comments: Vec<Comment> = (0..20)
            .map(|i| Comment {
                body: format!("comment {i}"),
                created_at: format!("2026-11-{i:02}"),
                user: None,
            })
            .collect();
        let prompt = issue_prompt(
            &issue("ENG-1", "Thing"),
            &comments,
            Some("  Start with the API.\n"),
        );
        assert!(prompt.starts_with("Work on Linear issue ENG-1: Thing"));
        assert!(prompt.contains("Do the thing"));
        assert!(prompt.contains("sam/eng-1129-flag"));
        assert!(prompt.contains("comment 19") && prompt.contains("comment 5"));
        assert!(!prompt.contains("comment 4\n"));
        assert!(prompt.ends_with("## Notes from me\n\nStart with the API.\n"));
    }
}
