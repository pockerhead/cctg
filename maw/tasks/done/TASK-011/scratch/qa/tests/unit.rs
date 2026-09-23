//! QA (TASK-011): pure checks on public registry/slots helpers.

use std::io::Write;

use cctg::hub::registry::{Registry, RegistryStore, folder_key, folder_name, topic_title};
use cctg::hub::slots::read_title;
use transcript::telegram_len;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "qa011u-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn incremental_title_scan_with_half_written_last_line() {
    let dir = temp_dir("title");
    let path = dir.join("t.jsonl");
    let head = "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n".repeat(1000);
    let title_line = "{\"type\":\"ai-title\",\"aiTitle\":\"Мой заголовок\"}\n";
    let (half1, half2) = title_line.split_at(20);
    std::fs::write(&path, format!("{head}{half1}")).unwrap();
    let p = path.to_str().unwrap();
    let (t, off) = read_title(p, 0);
    assert_eq!(t, None);
    assert_eq!(off as usize, head.len(), "partial line must not be counted");
    // Title in the already-scanned region is never re-read (by design).
    let (t2, off2) = read_title(p, off);
    assert_eq!((t2, off2), (None, off));
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(half2.as_bytes()).unwrap();
    drop(f);
    let (t3, _) = read_title(p, off);
    assert_eq!(t3.as_deref(), Some("Мой заголовок"));
    // A half-written line split inside a multi-byte char.
    let path2 = dir.join("t2.jsonl");
    let cut = title_line.find("й").unwrap() + 1; // inside the 2-byte 'й'
    std::fs::write(&path2, &title_line.as_bytes()[..cut]).unwrap();
    let p2 = path2.to_str().unwrap();
    let (t4, off4) = read_title(p2, 0);
    assert_eq!((t4, off4), (None, 0));
    std::fs::write(&path2, title_line).unwrap();
    assert_eq!(read_title(p2, off4).0.as_deref(), Some("Мой заголовок"));
    // Missing file.
    assert_eq!(read_title(dir.join("nope").to_str().unwrap(), 7), (None, 7));
    // A complete title line without the trailing newline is still found.
    let path3 = dir.join("t3.jsonl");
    std::fs::write(&path3, title_line.trim_end()).unwrap();
    assert!(read_title(path3.to_str().unwrap(), 0).0.is_some());
}

fn slot_json(folder: &str, ordinal: u32, topic: Option<i64>) -> String {
    format!(
        r#"{{"host":"box","folder_key":"{folder}","folder_name":"P","ordinal":{ordinal},"topic_id":{}}}"#,
        topic.map_or("null".to_owned(), |t| t.to_string())
    )
}

#[test]
fn load_rejects_duplicates_and_keeps_previous_on_torn_temp() {
    let dir = temp_dir("load");
    let store = RegistryStore::open(&dir).unwrap();
    let write = |slots: &[String]| {
        std::fs::write(
            dir.join("registry.json"),
            format!(r#"{{"version":1,"slots":[{}]}}"#, slots.join(",")),
        )
        .unwrap()
    };
    write(&[slot_json("c:/p", 1, Some(5)), slot_json("c:/p", 2, Some(6))]);
    assert!(store.load().is_ok());
    write(&[slot_json("c:/p", 1, Some(5)), slot_json("c:/q", 1, Some(5))]);
    let err = store.load().unwrap_err().to_string();
    assert!(!err.contains("c:/"), "{err}");
    write(&[slot_json("c:/p", 1, Some(5)), slot_json("c:/p", 1, Some(6))]);
    assert!(store.load().is_err());
    // Two slots without a topic yet are fine.
    write(&[slot_json("c:/p", 1, None), slot_json("c:/p", 2, None)]);
    assert!(store.load().is_ok());
    // Broken JSON: error text does not quote the content.
    std::fs::write(dir.join("registry.json"), "{\"version\":1,\"slots\":[{\"host\":\"SECRETHOST").unwrap();
    let err = format!("{:#}", store.load().unwrap_err());
    assert!(!err.contains("SECRETHOST"), "{err}");
    // Save then a torn temp: previous file loads.
    let reg = Registry::default();
    store.save(&RegistryStore::encode(&reg)).unwrap();
    std::fs::write(dir.join("registry.json.tmp"), "{\"version\":1,\"slo").unwrap();
    assert!(store.load().is_ok());
    // Save over an existing target works (Windows rename replaces).
    store.save(&RegistryStore::encode(&reg)).unwrap();
    assert!(store.load().is_ok());
}

#[test]
fn titles_fit_under_hostile_inputs() {
    let hosts = [
        "box".to_owned(),
        "h".repeat(200),
        "😀".repeat(50),
        "a\nb\tc".to_owned(),
        String::new(),
    ];
    let folders = [
        "Project".to_owned(),
        "Ж".repeat(300),
        "😀".repeat(100),
        "x".repeat(127),
        String::new(),
    ];
    let labels = [
        None,
        Some("😀".repeat(100)),
        Some("t".repeat(500)),
        Some("line1\nline2".to_owned()),
        Some(" ".to_owned()),
    ];
    for host in &hosts {
        for folder in &folders {
            for label in &labels {
                for ordinal in [1u32, 2, 99, u32::MAX] {
                    let t = topic_title(host, folder, ordinal, label.as_deref());
                    assert!(telegram_len(&t) <= 128, "{} > 128: {t}", telegram_len(&t));
                    assert!(t.starts_with('['));
                    if ordinal > 1 {
                        assert!(t.contains(&format!(" #{ordinal}")), "{t}");
                    }
                    assert!(!t.contains('\n'));
                }
            }
        }
    }
    let t = topic_title("box", "Project", 3, Some("My title"));
    assert_eq!(t, "[box] Project #3 · My title");
}

#[test]
fn folder_keys() {
    let k = folder_key(r"C:\Work\Project");
    for v in [
        r"c:/work/project/",
        r"\\?\C:\Work\Project",
        r"C:\WORK\PROJECT\\",
        r"\\?\c:\work\project\",
    ] {
        assert_eq!(folder_key(v), k, "{v}");
    }
    assert_eq!(folder_key(r"\\?\UNC\Srv\Share\D"), folder_key(r"\\srv\share\d\"));
    assert_ne!(folder_key("/home/U"), folder_key("/home/u"));
    assert_eq!(folder_name(r"\\?\C:\Work\Project\"), "Project");
    assert_eq!(folder_name(r"c:/work/project/"), "project");
    // no panics on odd input
    for odd in ["", "\\", "/", "\\\\?\\", "é\\?\\x", "\\\\?\\UNC", "C:", "C:\\"] {
        let _ = folder_key(odd);
        let _ = folder_name(odd);
    }
}
