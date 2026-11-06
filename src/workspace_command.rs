use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const PLACEHOLDERS: [&str; 6] = [
    "{issue}",
    "{identifier}",
    "{branch}",
    "{branch_words}",
    "{title}",
    "{dir}",
];

pub fn validate(field: &str, template: &[String]) -> Result<()> {
    let Some(program) = template.first() else {
        bail!("{field} must name a program");
    };
    if program.contains('{') {
        bail!("{field}'s program cannot be a placeholder");
    }
    for arg in template {
        let mut rest = arg.as_str();
        while let Some(start) = rest.find('{') {
            let Some(end) = rest[start..].find('}') else {
                break;
            };
            let placeholder = &rest[start..start + end + 1];
            if !PLACEHOLDERS.contains(&placeholder) || (field == "locate" && placeholder == "{dir}")
            {
                bail!(
                    "{placeholder} cannot be used in {field}; use {}",
                    PLACEHOLDERS.join(", ")
                );
            }
            rest = &rest[start + end + 1..];
        }
    }
    Ok(())
}

pub struct Values<'a> {
    pub identifier: &'a str,
    pub branch: &'a str,
    pub title: &'a str,
    pub dir: &'a Path,
}

fn branch_words(identifier: &str, branch: &str) -> Vec<String> {
    let prefix = format!("{}-", identifier.to_ascii_lowercase());
    let leaf = branch.rsplit('/').next().unwrap_or(branch);
    leaf.strip_prefix(&prefix)
        .unwrap_or("")
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn expand(template: &[String], values: &Values<'_>) -> Vec<String> {
    let issue = values.identifier.to_ascii_lowercase();
    let dir = values.dir.to_string_lossy();
    template
        .iter()
        .flat_map(|arg| {
            if arg == "{branch_words}" {
                return branch_words(values.identifier, values.branch);
            }
            let words = branch_words(values.identifier, values.branch).join(" ");
            vec![
                arg.replace("{issue}", &issue)
                    .replace("{identifier}", values.identifier)
                    .replace("{branch_words}", &words)
                    .replace("{branch}", values.branch)
                    .replace("{title}", values.title)
                    .replace("{dir}", &dir),
            ]
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct Located {
    pub dir: PathBuf,
    #[serde(default)]
    pub repos: Vec<LocatedRepo>,
}

#[derive(Debug, Deserialize)]
pub struct LocatedRepo {
    pub name: String,
    pub path: PathBuf,
}

pub fn locate(argv: &[String]) -> Result<Option<Located>> {
    let (program, args) = argv.split_first().context("locate is empty")?;
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run `{program}`"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let located: Located = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("`{}` did not print the expected JSON", argv.join(" ")))?;
    Ok(Some(located))
}

pub enum Finish<'a> {
    Dir(&'a Path),
    Locate {
        locate: &'a [String],
        linrex: &'a Path,
    },
}

fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

pub fn session_command(create: &[String], finish: &Finish<'_>, agent: &[String]) -> Vec<String> {
    let quote_all = |argv: &[String]| {
        argv.iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let create_line = quote_all(create);
    let program = shell_quote(create.first().map_or("", String::as_str));
    let on_failure = format!(
        r#"{{
    printf '\n%s failed. Fix the problem and run it again, or start the agent yourself.\n' {program}
    exec "${{SHELL:-sh}}" -l
  }}"#
    );
    let script = match finish {
        Finish::Dir(dir) => {
            let dir = shell_quote(&dir.to_string_lossy());
            format!(
                r#"if [ ! -d {dir} ]; then
  {create_line} || {on_failure}
fi
cd {dir} || exec "${{SHELL:-sh}}" -l
exec "$@"
"#
            )
        }
        Finish::Locate { locate, linrex } => {
            let locate_line = quote_all(locate);
            let locate_json =
                shell_quote(&serde_json::to_string(locate).unwrap_or_else(|_| "[]".to_owned()));
            let linrex = shell_quote(&linrex.to_string_lossy());
            format!(
                r#"if ! {locate_line} >/dev/null 2>&1; then
  {create_line} || {on_failure}
fi
exec {linrex} enter-workspace --locate {locate_json} -- "$@"
"#
            )
        }
    };
    let mut argv = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        script,
        "linrex".to_owned(),
    ];
    argv.extend(agent.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn values(dir: &Path) -> Values<'_> {
        Values {
            identifier: "ENG-7",
            branch: "sam/eng-7-fix-the-thing",
            title: "Fix the thing",
            dir,
        }
    }

    #[test]
    fn expands_placeholders_and_splits_branch_words() {
        let template: Vec<String> = [
            "tool",
            "new",
            "{issue}",
            "{branch_words}",
            "--title={title}",
            "--into",
            "{dir}",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            expand(&template, &values(Path::new("/w/eng-7"))),
            [
                "tool",
                "new",
                "eng-7",
                "fix",
                "the",
                "thing",
                "--title=Fix the thing",
                "--into",
                "/w/eng-7"
            ]
        );
        let other_branch = Values {
            branch: "unrelated",
            ..values(Path::new("/w"))
        };
        assert_eq!(
            expand(&["{branch_words}".into()], &other_branch),
            Vec::<String>::new()
        );
    }

    #[test]
    fn rejects_unknown_placeholders() {
        assert!(validate("create", &["tool".into(), "{issue}".into()]).is_ok());
        assert!(validate("create", &["tool".into(), "{ticket}".into()]).is_err());
        assert!(validate("create", &[]).is_err());
        assert!(validate("create", &["{dir}".into()]).is_err());
        assert!(validate("create", &["tool".into(), "{dir}".into()]).is_ok());
        assert!(validate("locate", &["tool".into(), "{dir}".into()]).is_err());
    }

    fn run(root: &Path, create: &[String]) -> (String, std::process::ExitStatus) {
        let agent = [
            "sh",
            "-c",
            "pwd; printf '%s\\n' \"$0\"",
            "the prompt with 'quotes', $dollars and `ticks`",
        ]
        .map(String::from);
        let dir = root.join("ws/eng-7");
        let argv = session_command(create, &Finish::Dir(&dir), &agent);
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .env("SHELL", "/usr/bin/true")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            output.status,
        )
    }

    fn creator(root: &Path, exit: i32) -> Vec<String> {
        let log = root.join("create.log");
        let dir = root.join("ws/eng-7");
        vec![
            "sh".into(),
            "-c".into(),
            format!(
                "echo \"$@\" >> '{}'; [ {exit} -eq 0 ] && mkdir -p '{}'; exit {exit}",
                log.display(),
                dir.display()
            ),
            "creator".into(),
            "it's".into(),
            "two words".into(),
        ]
    }

    #[test]
    fn creates_workspace_then_runs_agent_inside_it() {
        let root = tempfile::tempdir().unwrap();
        let (stdout, status) = run(root.path(), &creator(root.path(), 0));
        assert!(status.success());
        let log = std::fs::read_to_string(root.path().join("create.log")).unwrap();
        assert_eq!(log.trim(), "it's two words");
        let mut lines = stdout.lines();
        assert!(lines.next().unwrap().ends_with("ws/eng-7"));
        assert_eq!(
            lines.next().unwrap(),
            "the prompt with 'quotes', $dollars and `ticks`"
        );
    }

    #[test]
    fn existing_workspace_skips_the_command() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("ws/eng-7")).unwrap();
        let (stdout, _) = run(root.path(), &creator(root.path(), 0));
        assert!(!root.path().join("create.log").exists());
        assert!(stdout.lines().next().unwrap().ends_with("ws/eng-7"));
    }

    #[test]
    fn failed_command_falls_back_to_a_shell_without_starting_the_agent() {
        let root = tempfile::tempdir().unwrap();
        let (stdout, _) = run(root.path(), &creator(root.path(), 3));
        assert!(root.path().join("create.log").exists());
        assert!(stdout.contains("failed. Fix the problem"));
        assert!(!stdout.contains("the prompt"));
    }

    #[test]
    fn locate_reads_dir_and_repos_and_treats_failure_as_missing() {
        let found = locate(&[
            "sh".into(),
            "-c".into(),
            r#"printf '{"dir":"/w/eng-7","branch":"x","repos":[{"name":"api","path":"/w/eng-7/api"}]}'"#.into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(found.dir, Path::new("/w/eng-7"));
        assert_eq!(found.repos[0].name, "api");
        assert!(
            locate(&["sh".into(), "-c".into(), "exit 1".into()])
                .unwrap()
                .is_none()
        );
        assert!(locate(&["sh".into(), "-c".into(), "echo nope".into()]).is_err());
    }

    #[test]
    fn locate_script_creates_once_then_hands_over_to_linrex() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("ws/eng-7");
        let log = root.path().join("calls.log");
        let fake = |name: &str, body: &str| {
            let path = root.path().join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\necho \"{name} $*\" >> '{}'\n{body}\n",
                    log.display()
                ),
            )
            .unwrap();
            Command::new("chmod").arg("+x").arg(&path).status().unwrap();
            path.to_string_lossy().into_owned()
        };
        let locate_cmd = fake("locate", &format!("[ -d '{}' ]", dir.display()));
        let create_cmd = fake("create", &format!("mkdir -p '{}'", dir.display()));
        let linrex = fake("linrex", "");
        let locate_argv = vec![locate_cmd, "eng-7".to_owned()];
        let argv = session_command(
            &[create_cmd, "eng-7".into()],
            &Finish::Locate {
                locate: &locate_argv,
                linrex: Path::new(&linrex),
            },
            &["agent".into(), "it's the prompt".into()],
        );
        for _ in 0..2 {
            let status = Command::new(&argv[0]).args(&argv[1..]).status().unwrap();
            assert!(status.success());
        }
        let calls = std::fs::read_to_string(&log).unwrap();
        let calls: Vec<&str> = calls.lines().collect();
        let handover = format!(
            "linrex enter-workspace --locate [\"{}\",\"eng-7\"] -- agent it's the prompt",
            locate_argv[0]
        );
        assert_eq!(
            calls,
            [
                "locate eng-7",
                "create eng-7",
                handover.as_str(),
                "locate eng-7",
                handover.as_str(),
            ]
        );
    }
}
