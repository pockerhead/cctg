use std::process::Command;

#[test]
fn subcommands_do_not_write_to_stdout() {
    for args in [&["agent"][..], &["hook", "SessionStart"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_cctg"))
            .args(args)
            .output()
            .expect("cctg should start");

        assert!(
            output.status.success(),
            "cctg {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "cctg {args:?} wrote to stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn hub_without_config_fails_on_stderr_only() {
    // Empty working directory: no ./.env, and no CCTG_* variables inherited.
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("hub-no-config");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let output = Command::new(env!("CARGO_BIN_EXE_cctg"))
        .arg("hub")
        .current_dir(&dir)
        .env_remove("CCTG_BOT_TOKEN")
        .env_remove("CCTG_CHAT_ID")
        .env_remove("CCTG_ALLOWED_USER_IDS")
        .output()
        .expect("cctg should start");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CCTG_BOT_TOKEN is not set"), "{stderr}");
}
