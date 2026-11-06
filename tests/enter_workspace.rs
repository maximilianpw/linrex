use std::process::Command;

#[test]
fn starts_the_agent_in_the_located_workspace() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("ws/eng-7");
    std::fs::create_dir_all(&dir).unwrap();
    let locate = serde_json::to_string(&[
        "sh".to_owned(),
        "-c".to_owned(),
        format!(r#"printf '{{"dir":"{}","repos":[]}}'"#, dir.display()),
    ])
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_linrex"))
        .args([
            "enter-workspace",
            "--locate",
            &locate,
            "--",
            "sh",
            "-c",
            "pwd; echo \"$0\"",
            "a prompt",
        ])
        .env_remove("REX_SESSION")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    assert_eq!(
        std::fs::canonicalize(lines.next().unwrap()).unwrap(),
        std::fs::canonicalize(&dir).unwrap()
    );
    assert_eq!(lines.next(), Some("a prompt"));
}

#[test]
fn falls_back_to_a_shell_when_the_workspace_is_missing() {
    let locate = r#"["sh","-c","exit 1"]"#;
    let output = Command::new(env!("CARGO_BIN_EXE_linrex"))
        .args([
            "enter-workspace",
            "--locate",
            locate,
            "--",
            "sh",
            "-c",
            "echo agent-ran",
        ])
        .env("SHELL", "/usr/bin/true")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("agent-ran"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("found no workspace"));
}
