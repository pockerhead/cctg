use std::process::Command;

#[test]
fn subcommands_do_not_write_to_stdout() {
    for args in [&["hub"][..], &["agent"][..], &["hook", "SessionStart"][..]] {
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
