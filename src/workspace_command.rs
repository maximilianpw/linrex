use std::path::Path;

use anyhow::{Result, bail};

const PLACEHOLDERS: [&str; 6] = [
    "{issue}",
    "{identifier}",
    "{branch}",
    "{branch_words}",
    "{title}",
    "{dir}",
];

pub fn validate(template: &[String]) -> Result<()> {
    let Some(program) = template.first() else {
        bail!("create must name a program");
    };
    if program.contains('{') {
        bail!("create's program cannot be a placeholder");
    }
    for arg in template {
        let mut rest = arg.as_str();
        while let Some(start) = rest.find('{') {
            let Some(end) = rest[start..].find('}') else {
                break;
            };
            let placeholder = &rest[start..start + end + 1];
            if !PLACEHOLDERS.contains(&placeholder) {
                bail!(
                    "unknown placeholder {placeholder} in create; use {}",
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

fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

pub fn session_command(create: &[String], dir: &Path, agent: &[String]) -> Vec<String> {
    let dir = shell_quote(&dir.to_string_lossy());
    let create_line = create
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let program = shell_quote(create.first().map_or("", String::as_str));
    let script = format!(
        r#"if [ ! -d {dir} ]; then
  {create_line} || {{
    printf '\n%s failed. Fix the problem and run it again, or start the agent yourself.\n' {program}
    exec "${{SHELL:-sh}}" -l
  }}
fi
cd {dir} || exec "${{SHELL:-sh}}" -l
exec "$@"
"#
    );
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
        assert!(validate(&["tool".into(), "{issue}".into()]).is_ok());
        assert!(validate(&["tool".into(), "{ticket}".into()]).is_err());
        assert!(validate(&[]).is_err());
        assert!(validate(&["{dir}".into()]).is_err());
    }

    fn run(root: &Path, create: &[String]) -> (String, std::process::ExitStatus) {
        let agent = [
            "sh",
            "-c",
            "pwd; printf '%s\\n' \"$0\"",
            "the prompt with 'quotes', $dollars and `ticks`",
        ]
        .map(String::from);
        let argv = session_command(create, &root.join("ws/eng-7"), &agent);
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
}
