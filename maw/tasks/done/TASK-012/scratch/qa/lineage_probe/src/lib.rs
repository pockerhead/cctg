//! QA-only synthetic checks of `cctg::proctree::lineage` (TASK-012 QA).
#[cfg(test)]
mod tests {
    use cctg::proctree::{lineage, Lineage, Proc};

    const SID: &str = "qa-session";
    const STORE: &str = r"C:\Program Files\WindowsApps\Claude_2.7032.0.0_x64__pzs8sxrjxfjjc\app\claude.exe";
    const CLASSIC: &str = r"C:\Users\u\AppData\Local\AnthropicClaude\app-0.9.0\claude.exe";
    const BUNDLED: &str = r"C:\Users\u\AppData\Roaming\Claude\claude-code\2.1.280\claude.exe";
    const LOCAL: &str = r"C:\Users\u\.local\bin\claude.exe";

    fn p(pid: u32, name: &str, path: Option<&str>) -> Proc {
        Proc { pid, name: name.into(), image_path: path.map(Into::into) }
    }
    fn l(c: Option<u32>, pp: Option<u32>) -> Lineage {
        Lineage { claude_pid: c, parent_claude_pid: pp }
    }

    #[test]
    fn bundled_cli_under_store_desktop_is_top_level() {
        let chain = [p(1, "cctg.exe", None), p(2, "bash.exe", None), p(3, "claude.exe", Some(BUNDLED)), p(4, "claude.exe", Some(STORE))];
        assert_eq!(lineage(&chain, Some(3), Some(SID), SID), l(Some(3), None));
        assert_eq!(lineage(&chain, None, None, SID), l(Some(3), None));
        // stale CLAUDE_PID naming the Desktop main process
        assert_eq!(lineage(&chain, Some(4), Some(SID), SID), l(Some(3), None));
        // foreign env session naming Desktop: still no parent
        assert_eq!(lineage(&chain, Some(4), Some("other"), SID), l(Some(3), None));
    }

    #[test]
    fn classic_desktop_mixed_case_and_slashes_is_not_a_parent() {
        let upper = STORE.to_uppercase().replace(char::from(92u8), "/");
        let chain = [p(1, "cctg.exe", None), p(3, "CLAUDE.EXE", Some(LOCAL)), p(5, "explorer.exe", None), p(6, "Claude.exe", Some(&upper)), p(7, "claude.exe", Some(CLASSIC))];
        assert_eq!(lineage(&chain, Some(3), Some(SID), SID), l(Some(3), None));
    }

    #[test]
    fn real_cli_parent_above_desktop_hosted_chain_is_found() {
        let chain = [
            p(1, "cctg.exe", None), p(2, "bash.exe", None), p(3, "claude.exe", Some(LOCAL)),
            p(4, "bash.exe", None), p(5, "claude.exe", Some(BUNDLED)), p(6, "claude.exe", Some(STORE)),
        ];
        assert_eq!(lineage(&chain, Some(3), Some(SID), SID), l(Some(3), Some(5)));
    }

    #[test]
    fn desktop_between_own_and_real_parent_is_skipped() {
        // Contrived: a Desktop image between two CLIs must not stop the search.
        let chain = [p(1, "cctg.exe", None), p(3, "claude.exe", Some(LOCAL)), p(6, "claude.exe", Some(STORE)), p(8, "claude.exe", Some(LOCAL))];
        assert_eq!(lineage(&chain, Some(3), Some(SID), SID), l(Some(3), Some(8)));
    }

    #[test]
    fn only_desktop_in_chain_uses_env_pid() {
        // The hook's own claude missing from the verified chain (truncated).
        let chain = [p(1, "cctg.exe", None), p(2, "bash.exe", None)];
        assert_eq!(lineage(&chain, Some(3), Some(SID), SID), l(Some(3), None));
        assert_eq!(lineage(&chain, None, None, SID), l(None, None));
    }

    #[test]
    fn linux_names_without_paths_still_work() {
        let chain = [p(1, "cctg", None), p(2, "bash", None), p(3, "claude", None), p(4, "bash", None), p(5, "claude", None)];
        assert_eq!(lineage(&chain, Some(3), Some(SID), SID), l(Some(3), Some(5)));
    }
}
