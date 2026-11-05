use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, Deserialize)]
pub struct RexSession {
    pub session_id: String,
    pub label: String,
}

pub struct NewSession<'a> {
    pub label: &'a str,
    pub cwd: &'a Path,
    pub window: &'a str,
    pub command: &'a [String],
}

#[derive(Debug, Clone, Default)]
pub struct Rex;

impl Rex {
    fn run<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T> {
        let output = Command::new("rex")
            .args(args)
            .output()
            .context("could not run `rex`; is it on PATH?")?;
        if !output.status.success() {
            bail!(
                "`rex {}` failed: {}",
                args.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("unexpected output from `rex {}`", args.join(" ")))
    }

    pub fn sessions(&self) -> Result<Vec<RexSession>> {
        #[derive(Deserialize)]
        struct Listing {
            sessions: Vec<RexSession>,
        }
        Ok(self.run::<Listing>(&["ls", "--json"])?.sessions)
    }

    pub fn create_session(&self, new: &NewSession<'_>) -> Result<String> {
        #[derive(Deserialize)]
        struct Created {
            session_id: String,
        }
        let cwd = new
            .cwd
            .to_str()
            .context("checkout path is not valid UTF-8")?;
        let mut args = vec![
            "new", new.label, "-c", cwd, "--window", new.window, "--json", "--",
        ];
        args.extend(new.command.iter().map(String::as_str));
        Ok(self.run::<Created>(&args)?.session_id)
    }

    pub fn show_session(&self, session_id: &str) -> Result<()> {
        let client = self.app_client()?;
        let target = format!("session_id={session_id}");
        let mut args: Vec<&str> = Vec::new();
        if let Some(client) = &client {
            args.extend(["-C", client]);
        }
        args.extend(["do", "session.select", &target]);
        let output = Command::new("rex")
            .args(&args)
            .output()
            .context("could not run `rex`")?;
        if !output.status.success() {
            bail!(
                "could not switch the Rex app to the session: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn app_client(&self) -> Result<Option<String>> {
        if std::env::var_os("REX_CLIENT").is_some() {
            return Ok(None);
        }
        #[derive(Deserialize)]
        struct Listing {
            clients: Vec<Client>,
        }
        let listing: Listing = self.run(&["client", "ls", "--json"])?;
        Ok(pick_app_client(&listing.clients).map(|c| c.client_id.clone()))
    }
}

#[derive(Debug, Deserialize)]
struct Client {
    client_id: String,
    info: Option<ClientInfo>,
    connection_state: String,
}

#[derive(Debug, Deserialize)]
struct ClientInfo {
    kind: String,
    platform: String,
}

fn pick_app_client(clients: &[Client]) -> Option<&Client> {
    let apps: Vec<&Client> = clients
        .iter()
        .filter(|c| c.connection_state == "connected")
        .filter(|c| c.info.as_ref().is_some_and(|i| i.kind == "app"))
        .collect();
    let local = std::env::consts::OS;
    apps.iter()
        .find(|c| c.info.as_ref().is_some_and(|i| i.platform == local))
        .or(apps.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: &str, kind: &str, platform: &str, state: &str) -> Client {
        Client {
            client_id: id.into(),
            info: Some(ClientInfo {
                kind: kind.into(),
                platform: platform.into(),
            }),
            connection_state: state.into(),
        }
    }

    #[test]
    fn prefers_connected_app_on_this_platform() {
        let clients = [
            client("cli", "cli", std::env::consts::OS, "connected"),
            client("phone", "app", "ios", "connected"),
            client("stale", "app", std::env::consts::OS, "lost"),
            client("desk", "app", std::env::consts::OS, "connected"),
        ];
        assert_eq!(pick_app_client(&clients).unwrap().client_id, "desk");
    }

    #[test]
    fn falls_back_to_any_connected_app() {
        let clients = [client("phone", "app", "ios", "connected")];
        assert_eq!(pick_app_client(&clients).unwrap().client_id, "phone");
        assert!(pick_app_client(&[client("cli", "cli", "macos", "connected")]).is_none());
    }
}
