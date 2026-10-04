//! `cctg sandbox on|off|status` and the wrapper's `cctg sandbox-check`.
//!
//! These print to the local terminal only, so they may name the folder.

use std::path::{Path, PathBuf};

use super::marks::{self, Removed};
use super::preflight::{self, Probe, RealProbe};
use super::{paths, profile};

/// `sandbox-check`: the folder is not marked; the wrapper starts claude as
/// before.
pub const NOT_MARKED: i32 = 10;
/// `sandbox-check`: anything else that is not a profile; the wrapper does not
/// start claude.
pub const CHECK_FAILED: i32 = 3;

/// The folder named, or the current directory, resolved.
fn folder_of(folder: Option<PathBuf>) -> Option<PathBuf> {
    let folder = match folder {
        Some(folder) => folder,
        None => std::env::current_dir().ok()?,
    };
    paths::canonical(&folder)
}

fn marks_file(probe: &dyn Probe) -> Option<PathBuf> {
    probe.home().map(|home| super::marks_file(&home))
}

/// `cctg sandbox on [--folder F]`.
pub fn on(folder: Option<PathBuf>) -> i32 {
    let probe = RealProbe;
    let Some(folder) = folder_of(folder) else {
        eprintln!("cctg sandbox: папка не найдена");
        return 1;
    };
    let checked = preflight::check(&probe, &folder).and_then(|_| preflight::wrapper(&probe));
    if let Err(refusal) = checked {
        eprintln!("cctg sandbox: {}: {refusal}", folder.display());
        return 1;
    }
    let Some(file) = marks_file(&probe) else {
        eprintln!("cctg sandbox: {}", preflight::Refusal::NoHome);
        return 1;
    };
    #[cfg(windows)]
    {
        use super::switch::{self, WinOnError, WinStep};
        let mut step = |step: WinStep| match step {
            WinStep::Granting => println!(
                "Выдаю права учётке сэндбокса на {}; на больших папках это может занять минуты…",
                folder.display()
            ),
            WinStep::Seen(seen) => eprintln!("  … {seen} файлов"),
        };
        let home = super::home_dir_of(&|n| std::env::var(n).ok());
        let exe = std::env::current_exe().ok();
        match switch::windows_on(&file, &folder, home.as_deref(), exe.as_deref(), &mut step) {
            Ok(count) => {
                if count > 0 {
                    println!("Запрещена запись {count} файлам с несколькими жёсткими ссылками.");
                }
            }
            Err(WinOnError::Nested) => {
                eprintln!(
                    "cctg sandbox: {}: {}",
                    folder.display(),
                    preflight::Refusal::BadFolder(preflight::FolderProblem::Nested)
                );
                return 1;
            }
            Err(WinOnError::Refused(refusal)) => {
                eprintln!("cctg sandbox: {refusal}");
                return 1;
            }
            Err(WinOnError::GrantFailed) => {
                eprintln!("cctg sandbox: не удалось выдать права; повторите cctg sandbox on");
                return 1;
            }
            Err(WinOnError::StampFailed) => {
                eprintln!(
                    "cctg sandbox: не удалось защитить служебные файлы; повторите cctg sandbox on"
                );
                return 1;
            }
        }
    }
    if let Err(error) = marks::add(&file, &folder) {
        eprintln!("cctg sandbox: {error}");
        return 1;
    }
    println!(
        "Сэндбокс включён для {}. Новые сессии claude-cctg в ней и во вложенных папках \
         стартуют в сэндбоксе; уже запущенные перейдут после «⬆️ Обновить» в меню бота \
         или перезапуска.",
        folder.display()
    );
    0
}

/// `cctg sandbox off [--folder F]`.
pub fn off(folder: Option<PathBuf>) -> i32 {
    let probe = RealProbe;
    let (Some(folder), Some(file)) = (folder_of(folder), marks_file(&probe)) else {
        eprintln!("cctg sandbox: папка или домашний каталог не найдены");
        return 1;
    };
    match marks::remove(&file, &folder) {
        Ok(Removed::Exact) => {
            // Windows: list slot-owned protected names, revoke the tree ACEs
            // and retire the slot (a new owner gets a fresh SID).
            #[cfg(windows)]
            if let Some(home) = super::home_dir_of(&|n| std::env::var(n).ok()) {
                let owned = super::switch::windows_owned(&folder, &home);
                if !owned.is_empty() {
                    println!(
                        "Эти служебные файлы создала команда из сэндбокса; проверьте их перед \
                         запуском claude без сэндбокса:"
                    );
                    for path in &owned {
                        println!("  {}", path.display());
                    }
                }
                let errors = super::switch::windows_release(&folder, &home);
                if errors > 0 {
                    println!("Права слота сняты с папки (ошибок: {errors}).");
                }
            }
            println!(
                "Сэндбокс выключен для {}. Запущенные сессии выйдут из него после \
                 «⬆️ Обновить» или перезапуска.",
                folder.display()
            );
            0
        }
        Ok(Removed::Ancestor(mark)) => {
            eprintln!(
                "cctg sandbox: {} внутри помеченной папки {}; выключите там",
                folder.display(),
                mark.display()
            );
            1
        }
        Ok(Removed::None) => {
            println!("Сэндбокс для {} не был включён.", folder.display());
            0
        }
        Err(error) => {
            eprintln!("cctg sandbox: {error}");
            1
        }
    }
}

/// `cctg sandbox status [--folder F]`.
pub fn status(folder: Option<PathBuf>) -> i32 {
    let probe = RealProbe;
    let Some(folder) = folder_of(folder) else {
        eprintln!("cctg sandbox: папка не найдена");
        return 1;
    };
    match marks_file(&probe).map(|file| marks::load(&file)) {
        None => println!("{}: сэндбокс выключен", folder.display()),
        // Damaged: decided as `sandbox-check` decides (marks module docs).
        Some(Err(error)) => {
            let file = marks_file(&probe).unwrap_or_default();
            let state = match marks::covered(&file, &folder) {
                Ok(true) => "сэндбокс включён (по повреждённому файлу или его копии .bak)",
                Ok(false) => "сэндбокс выключен (файл повреждён, но эту папку не называет)",
                Err(_) => "не определить, claude-cctg здесь не стартует",
            };
            println!("{}: {error}; {state}", folder.display());
        }
        Some(Ok(list)) => match marks::covering(&list, &folder) {
            Some(mark) if paths::within(&folder, mark) => {
                println!("{}: сэндбокс включён", folder.display())
            }
            Some(mark) => println!(
                "{}: сэндбокс включён меткой папки {}",
                folder.display(),
                mark.display()
            ),
            None => println!("{}: сэндбокс выключен", folder.display()),
        },
    }
    #[cfg(windows)]
    windows_status(&folder);
    match preflight::check(&probe, &folder).and_then(|ready| {
        preflight::wrapper(&probe)?;
        Ok(ready)
    }) {
        Ok(ready) => println!("устройство готово (Claude Code {})", ready.claude),
        Err(refusal) => println!("устройство не готово: {refusal}"),
    }
    0
}

/// Prints the folder's slot and its state (Windows).
#[cfg(windows)]
fn windows_status(folder: &Path) {
    use crate::sandbox::win;
    let Some(home) = crate::sandbox::home_dir_of(&|n| std::env::var(n).ok()) else {
        return;
    };
    let win_dir = win::win_dir(&home);
    let folder_str = folder.to_string_lossy().to_string();
    match win::slots::active_slot(&win_dir, &folder_str) {
        Ok(Some(k)) => println!("слот сэндбокса: #{k} (активен)"),
        Ok(None) => println!("слот сэндбокса: не назначен"),
        Err(_) => println!("слот сэндбокса: файл слотов не читается"),
    }
}

/// `cctg sandbox-check --settings S` in the session folder (the current
/// directory): the profile's path on stdout and 0, [`NOT_MARKED`] with empty
/// stdout, or [`CHECK_FAILED`] with a reason on stderr. With `cmd`, the `.cmd`
/// wrapper form: `profile <path>` (0), `unmarked` (10), empty (3).
pub fn check(settings: &Path, probe_run: bool, cmd: bool) -> i32 {
    let cwd = std::env::current_dir().ok();
    if probe_run {
        eprintln!("cctg sandbox-check: a probe profile (TASK-087), not for real sessions");
        return check_in(&RealProbe, settings, cwd, true, cmd);
    }
    check_in(&RealProbe, settings, cwd, false, cmd)
}

pub fn check_with(probe: &dyn Probe, settings: &Path, cwd: Option<PathBuf>) -> i32 {
    check_in(probe, settings, cwd, false, false)
}

/// [`check_with`] in the `.cmd` output form (for the install_e2e tests).
pub fn check_with_cmd(probe: &dyn Probe, settings: &Path, cwd: Option<PathBuf>) -> i32 {
    check_in(probe, settings, cwd, false, true)
}

fn check_in(
    probe: &dyn Probe,
    settings: &Path,
    cwd: Option<PathBuf>,
    probe_run: bool,
    cmd: bool,
) -> i32 {
    let unmarked = |code: i32| {
        if cmd {
            println!("unmarked");
        }
        code
    };
    let Some(folder) = cwd.and_then(|cwd| paths::canonical(&cwd)) else {
        eprintln!("cctg sandbox-check: the current folder cannot be resolved");
        return CHECK_FAILED;
    };
    // No home: no marks can exist.
    let Some(file) = marks_file(probe) else {
        return unmarked(NOT_MARKED);
    };
    match marks::covered(&file, &folder) {
        Ok(false) => {
            if marks::damaged(&file) {
                eprintln!(
                    "cctg sandbox-check: the sandbox marks file is damaged; it does not name \
                     this folder, which starts as usual (cctg doctor)"
                );
            }
            return unmarked(NOT_MARKED);
        }
        Ok(true) => {}
        Err(error) => {
            eprintln!(
                "cctg sandbox-check: {error}; this folder may be marked, so claude does not \
                 start (cctg doctor)"
            );
            return CHECK_FAILED;
        }
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => {
            eprintln!("cctg sandbox-check: own executable not found");
            return CHECK_FAILED;
        }
    };
    let prepared = if probe_run {
        profile::prepare_probe_run(probe, settings, &folder, &exe)
    } else {
        profile::prepare(probe, settings, &folder, &exe)
    };
    match prepared {
        Ok(path) => {
            if cmd {
                println!("profile {}", path.display());
            } else {
                println!("{}", path.display());
            }
            0
        }
        Err(refusal) => {
            eprintln!("cctg sandbox-check: сессия не запущена: {refusal}");
            CHECK_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::preflight::tests::{Fake, home_and_folder};

    #[test]
    fn check_answers_by_mark() {
        let (_dir, home, folder) = home_and_folder("cli-check");
        let conf = home.join(".cctg").join("claude");
        std::fs::create_dir_all(&conf).unwrap();
        let settings = conf.join("settings.json");
        std::fs::write(&settings, b"{}").unwrap();
        let fake = Fake::linux(&home);
        assert_eq!(
            check_with(&fake, &settings, Some(folder.clone())),
            NOT_MARKED
        );
        let file = crate::sandbox::marks_file(&home);
        marks::add(&file, &folder).unwrap();
        assert_eq!(check_with(&fake, &settings, Some(folder.clone())), 0);
        assert!(profile::profile_path(&settings, &folder).is_file());
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        assert_eq!(
            check_with(&fake, &settings, Some(folder.join("sub"))),
            0,
            "a subfolder is covered"
        );
        // A damaged file: the backup of the last save still marks it.
        std::fs::write(&file, b"{broken").unwrap();
        assert_eq!(check_with(&fake, &settings, Some(folder.clone())), 0);
        // Without the backup a cut-off file decides nothing: no start.
        std::fs::remove_file(file.with_file_name("folders.json.bak")).unwrap();
        assert_eq!(
            check_with(&fake, &settings, Some(folder.clone())),
            CHECK_FAILED
        );
        // Review finding 4: damaged but whole, and not naming this folder:
        // it starts as before.
        std::fs::write(&file, b"{\"version\":1,\"folders\":[],}").unwrap();
        assert_eq!(
            check_with(&fake, &settings, Some(folder.clone())),
            NOT_MARKED
        );
        std::fs::remove_file(&file).unwrap();
        marks::add(&file, &folder).unwrap();
        let mut old = Fake::linux(&home);
        old.answer("claude", 0, "2.1.200 (Claude Code)");
        assert_eq!(check_with(&old, &settings, Some(folder)), CHECK_FAILED);
    }
}
