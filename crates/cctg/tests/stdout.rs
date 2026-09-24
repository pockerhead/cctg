mod common;

#[test]
fn subcommands_do_not_write_to_stdout() {
    // Its own home and no session variables: this `cctg agent` once reached
    // the live hub as the session that ran the tests (TASK-042).
    let home = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("stdout-home");
    std::fs::create_dir_all(&home).expect("temp home");
    for args in [&["agent"][..], &["hook", "SessionStart"][..]] {
        let output = common::cctg(&home)
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
    let output = common::cctg(&dir)
        .arg("hub")
        .current_dir(&dir)
        .output()
        .expect("cctg should start");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CCTG_BOT_TOKEN is not set"), "{stderr}");
}

#[test]
fn malformed_env_file_does_not_echo_its_contents() {
    // Synthetic values only. The unterminated quote makes dotenvy fail on the
    // token line; its parse error would quote that line and everything after it.
    let secret = format!("env-secret-marker-{}", std::process::id());
    let user_id = format!("98765{}", std::process::id());
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("hub-bad-env");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let env_file = dir.join("bad.env");
    std::fs::write(
        &env_file,
        format!(
            "CCTG_CHAT_ID=-1001\nCCTG_BOT_TOKEN='777:{secret}\nCCTG_ALLOWED_USER_IDS={user_id}\n"
        ),
    )
    .expect("write bad.env");

    let output = common::cctg(&dir)
        .arg("hub")
        .arg("--env-file")
        .arg(&env_file)
        .current_dir(&dir)
        .output()
        .expect("cctg should start");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bad.env"), "{stderr}");
    for leaked in [
        secret.as_str(),
        user_id.as_str(),
        "777:",
        "CCTG_ALLOWED_USER_IDS=",
    ] {
        assert!(!stderr.contains(leaked), "{leaked} leaked: {stderr}");
    }
}
