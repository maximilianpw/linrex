mod checkout;
mod config;
mod issue_session;
mod linear;
mod rex;
mod tui;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use checkout::Repo;
use config::{Config, RepoMatch};
use issue_session::{Opened, StartOptions, StatusChange};
use linear::LinearClient;

#[derive(Parser)]
#[command(
    version,
    about = "Linear issues across workspaces, with a Rex session per issue"
)]
struct Cli {
    #[arg(long, global = true, env = "LINREX_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print your active assigned issues in every workspace
    List,
    /// Switch to the Rex session for an issue, creating its checkout and session if needed
    Start {
        identifier: String,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Print the config file path
    ConfigPath,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = match cli.config {
        Some(path) => path,
        None => Config::default_path()?,
    };
    match cli.command {
        Some(Command::ConfigPath) => {
            println!("{}", path.display());
            Ok(())
        }
        None => tui::run(Config::load(&path)?),
        Some(Command::List) => list(&Config::load(&path)?),
        Some(Command::Start { identifier, repo }) => {
            start(&Config::load(&path)?, &identifier, repo)
        }
    }
}

fn clients(config: &Config) -> impl Iterator<Item = (&str, Result<LinearClient>)> {
    config.workspaces.iter().map(|w| {
        let client = config
            .api_key(w)
            .and_then(|key| LinearClient::new(key).map_err(Into::into));
        (w.name.as_str(), client)
    })
}

fn list(config: &Config) -> Result<()> {
    let sessions = rex::Rex.sessions().unwrap_or_default();
    let mut failed = false;
    for (name, client) in clients(config) {
        let snapshot = match client.and_then(|c| c.snapshot().map_err(Into::into)) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                eprintln!("{name}: {error:#}");
                failed = true;
                continue;
            }
        };
        println!(
            "{name} ({}) as {}",
            snapshot.organization, snapshot.viewer.display_name
        );
        for issue in snapshot.issues.iter().filter(|i| {
            i.assignee
                .as_ref()
                .is_some_and(|a| a.id == snapshot.viewer.id)
        }) {
            let session =
                issue_session::find_session(&sessions, &issue.identifier).map_or("", |_| "●");
            println!(
                "  {session:1} {:<10} {:<14} {:<11} {}",
                issue.identifier,
                issue.state.name,
                issue.priority.label(),
                issue.title
            );
        }
    }
    if failed {
        bail!("some workspaces could not be loaded");
    }
    Ok(())
}

fn start(config: &Config, identifier: &str, repo: Option<PathBuf>) -> Result<()> {
    let mut errors = Vec::new();
    let found = clients(config).find_map(|(name, client)| {
        let result = client.and_then(|c| {
            c.issue(identifier)
                .map(|issue| (name, c, issue))
                .map_err(Into::into)
        });
        result
            .map_err(|e| errors.push(format!("{name}: {e:#}")))
            .ok()
    });
    let Some((workspace, client, issue)) = found else {
        bail!("{identifier} was not found:\n  {}", errors.join("\n  "));
    };
    let repo = match repo {
        Some(path) => {
            let path = std::path::absolute(&path)?;
            config
                .repos
                .iter()
                .map(|r| &r.repo)
                .find(|r| r.path == path)
                .cloned()
                .unwrap_or_else(|| Repo::at(path))
        }
        None => match config.repo_for(workspace, &issue.team.key, issue.project.as_deref()) {
            RepoMatch::One(repo) => repo.clone(),
            RepoMatch::Ambiguous(repos) => bail!(
                "several repos match team {}; pass --repo with one of: {}",
                issue.team.key,
                repos
                    .iter()
                    .map(|r| r.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            RepoMatch::Unmapped => bail!(
                "no repo is mapped to team {}; add a [[repos]] entry or pass --repo",
                issue.team.key
            ),
        },
    };
    let rex = rex::Rex;
    let opened = issue_session::start(
        &rex,
        &client,
        &issue,
        &StartOptions {
            repo: &repo,
            command: &config.session.command,
            move_to_started: config.session.move_to_started,
        },
    )?;
    match &opened {
        Opened::Existing { label, .. } => println!("Switching to existing session {label}"),
        Opened::Created {
            label,
            checkout,
            checkout_created,
            status,
            ..
        } => {
            let created = if *checkout_created { "new " } else { "" };
            println!(
                "Created session {label} in {created}checkout {}",
                checkout.display()
            );
            match status {
                StatusChange::Moved(state) => println!("Moved {} to {state}", issue.identifier),
                StatusChange::Failed(error) => eprintln!("Could not change the status: {error}"),
                StatusChange::Unchanged => {}
            }
        }
    }
    rex.show_session(opened.session_id())
        .context("the session exists, but the Rex app did not switch to it")
}
