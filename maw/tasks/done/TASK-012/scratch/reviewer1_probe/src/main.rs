use cctg::proctree::{lineage, Proc};

fn chain(nodes: &[(u32, &str)]) -> Vec<Proc> {
    nodes
        .iter()
        .map(|&(pid, name)| Proc { pid, name: name.into() })
        .collect()
}

fn main() {
    let sid = "session";
    let native = chain(&[
        (1, "cctg.exe"),
        (2, "bash.exe"),
        (30, "claude.exe"),
        (20, "bash.exe"),
        (10, "claude.exe"),
    ]);
    let npm_only = chain(&[
        (1, "cctg.exe"),
        (2, "bash.exe"),
        (30, "node.exe"),
        (20, "bash.exe"),
        (10, "node.exe"),
    ]);
    let mixed = chain(&[
        (1, "cctg.exe"),
        (2, "bash.exe"),
        (30, "node.exe"),
        (20, "bash.exe"),
        (10, "claude.exe"),
    ]);

    println!("native={:?}", lineage(&native, Some(30), Some(sid), sid));
    println!("npm_only={:?}", lineage(&npm_only, Some(30), Some(sid), sid));
    println!("mixed={:?}", lineage(&mixed, Some(30), Some(sid), sid));
}
