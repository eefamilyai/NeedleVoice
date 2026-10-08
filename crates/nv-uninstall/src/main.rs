//! NeedleVoice uninstaller.
//!
//! This is a program of its own, and a small one. It used to be the installer
//! under another name — the setup executable copied itself into the install
//! folder as `Uninstall.exe`, so both files were byte-for-byte the same ~150 MB
//! binary with the entire compressed payload inside it. Windows Defender's
//! machine-learning heuristic reads that combination — a big unsigned
//! executable carrying a compressed archive that drops other executables, plus
//! a `cmd.exe` line to delete the folder it lives in — as a dropper, and
//! flagged both copies.
//!
//! So this program:
//!
//! * embeds no payload and writes no executable anywhere;
//! * stays a few megabytes, most of which is the window toolkit;
//! * creates no windows and no windows message loop — it prints, does its work
//!   and exits;
//! * does not try to delete a file it is still executing. It renames itself out
//!   of the way, and the *copy* it leaves running in the temporary folder
//!   removes the rest. Nothing self-destructs.
//!
//! `docs/installer.md` explains the reasoning for anyone tempted to fold this
//! back into the installer.

#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nv_core::{paths, win};

fn main() {
    let code = run();
    std::process::exit(code);
}

fn run() -> i32 {
    let args: Args = Args::parse(std::env::args().skip(1));
    if args.help {
        usage();
        return 0;
    }

    // A copy of ourselves running from the temporary folder: wait for the
    // original to exit, then finish the job. This is the only case where the
    // uninstaller runs without a console to write to.
    if let Some(parent) = args.parent {
        wait_for_exit(parent);
        clean_up(args.dir.as_deref(), true);
        return 0;
    }

    let Some(dir) = resolve_dir(&args) else {
        complain("Couldn't work out where NeedleVoice is installed. Pass --dir <folder>.");
        return 2;
    };
    if !dir.is_dir() {
        complain(&format!("{} is not a folder.", dir.display()));
        return 2;
    }

    // Everything is open, so the microphone and the speakers are released and
    // the agent is not left running out of a folder that is being deleted.
    stop_agent();

    if !delete_user_data(&dir, args.delete_settings) {
        return 1;
    }
    remove_shortcuts();

    // Hand over to a copy of ourselves running out of the temporary folder, and
    // let *it* delete the folder we are standing in. Nothing here has to delete
    // a file it is executing, and nothing renames itself out of the way before
    // the handover has worked.
    let me = std::env::current_exe().ok();
    if let Some(me) = &me {
        match relaunch_from_temp(me, &dir) {
            Ok(()) => return 0,
            Err(e) => complain(&format!("Couldn't start the cleanup pass: {e}")),
        }
    }

    // The handover failed. Rename ourselves aside — Windows will not let a
    // running program be deleted, but it will let it be renamed — and take as
    // much of the folder as will go.
    if let Some(me) = &me {
        if let Err(e) = move_self_aside(me) {
            complain(&e);
        }
    }
    clean_up(Some(&dir), false);
    1
}

// ── arguments ───────────────────────────────────────────────────────────────

struct Args {
    dir: Option<PathBuf>,
    delete_settings: bool,
    parent: Option<u32>,
    help: bool,
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>) -> Args {
        let mut a = Args { dir: None, delete_settings: false, parent: None, help: false };
        let mut it = argv.peekable();
        while let Some(arg) = it.next() {
            // Add/Remove Programs and the shortcut both use "--flag"; "/flag" is
            // accepted because that is what some launchers rewrite it to.
            let flag = arg.trim_start_matches(['-', '/']).to_ascii_lowercase();
            match flag.as_str() {
                "uninstall" | "remove" | "" => {}
                "quiet" | "silent" | "s" => {}
                "delete-settings" | "deletesettings" => a.delete_settings = true,
                "dir" | "d" | "install-dir" => a.dir = it.next().map(PathBuf::from),
                "parent" => a.parent = it.next().and_then(|v| v.parse().ok()),
                "help" | "h" | "?" => a.help = true,
                _ => {}
            }
        }
        a
    }
}

fn usage() {
    let text = "NeedleVoice uninstaller\n\n\
                Usage: Uninstall.exe [--dir <folder>] [--delete-settings]\n\n\
                \x20 --dir <folder>      where NeedleVoice is installed\n\
                \x20 --delete-settings   also remove the saved settings and schedule\n";
    report(text.trim_end());
}

// ── where ───────────────────────────────────────────────────────────────────

fn resolve_dir(args: &Args) -> Option<PathBuf> {
    if let Some(d) = &args.dir {
        return Some(d.clone());
    }
    // Asked of the registry first, the way Add/Remove Programs does it.
    if let Some(d) = win::installed_location() {
        return Some(d);
    }
    // Then the place our own executable lives, if it looks like ours.
    let here = std::env::current_exe().ok()?;
    let dir = here.parent()?.to_path_buf();
    let ours = dir.join(nv_core::AGENT_EXE).exists() || dir.join(nv_core::CONFIG_EXE).exists();
    ours.then_some(dir)
}

// ── steps ───────────────────────────────────────────────────────────────────

/// Ask the agent and the settings app to close, then wait. Falling back to
/// terminating them is the installer-of-last-resort, not the first move.
fn stop_agent() {
    for exe in [nv_core::AGENT_EXE, nv_core::CONFIG_EXE] {
        if !win::close_processes_named(exe, Duration::from_secs(5)) {
            report(&format!("{exe} did not close and was stopped"));
        }
    }
    // Give the audio and microphone handles a moment to go.
    std::thread::sleep(Duration::from_millis(300));
}

/// The two registry keys and the shortcuts: all of the "uninstall it" plumbing
/// that Add/Remove Programs knows about.
fn remove_shortcuts() {
    let _ = win::set_autostart(false, Path::new(""));
    let _ = std::fs::remove_file(desktop_link());
    let _ = std::fs::remove_dir_all(start_menu_dir());
    win::unregister_uninstaller();
}

fn delete_user_data(dir: &Path, delete_settings: bool) -> bool {
    if !delete_settings {
        return true;
    }
    let data = paths::data_dir();
    // Never remove the folder we were asked to empty if they happen to be the
    // same place.
    if data == dir {
        return true;
    }
    if !data.exists() {
        return true;
    }
    match std::fs::remove_dir_all(&data) {
        Ok(()) => {
            report(&format!("Removed settings in {}", data.display()));
            true
        }
        Err(e) => {
            complain(&format!("Couldn't remove {}: {e}", data.display()));
            false
        }
    }
}

/// Rename the running executable out of the way. Returns the file's new path.
///
/// Renaming is permitted for a running image where deleting is not, and it is
/// how the folder the file sits in becomes removable — in the install directory
/// and in the temporary cleanup copy alike. (An earlier version skipped the
/// rename when the file was already in the temporary folder, on the theory that
/// it had "nothing to get out of the way of"; the folder could then not be
/// removed, and every uninstall left one behind.)
///
/// A rename can still be refused for a moment right after the file was written —
/// a virus scanner or the search indexer will have it open — so it is retried
/// rather than given up on.
fn move_self_aside(me: &Path) -> Result<PathBuf, String> {
    let name = me.file_name().unwrap_or_default().to_string_lossy().to_string();
    if name.ends_with(".old") {
        return Ok(me.to_path_buf());
    }
    let mut aside = me.to_path_buf().into_os_string();
    aside.push(".old");
    let aside = PathBuf::from(aside);
    let _ = std::fs::remove_file(&aside);

    let mut last = String::new();
    for attempt in 0..10 {
        match std::fs::rename(me, &aside) {
            Ok(()) => return Ok(aside),
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
        }
    }
    Err(format!("renaming {}: {last}", me.display()))
}

/// Copy ourselves somewhere Windows does not mind us exiting from, run that
/// copy, and let it do the deleting. Nothing has to delete itself.
fn relaunch_from_temp(moved: &Path, dir: &Path) -> Result<(), String> {
    let temp = std::env::temp_dir();
    std::fs::create_dir_all(&temp).map_err(|e| e.to_string())?;
    let copy_dir = temp.join(format!("NeedleVoice-uninstall-{}", std::process::id()));
    std::fs::create_dir_all(&copy_dir).map_err(|e| e.to_string())?;
    let copy = copy_dir.join(nv_core::UNINSTALL_EXE);
    std::fs::copy(moved, &copy).map_err(|e| format!("copying the uninstaller: {e}"))?;

    let mut cmd = std::process::Command::new(&copy);
    cmd.arg("--parent").arg(std::process::id().to_string());
    cmd.arg("--dir").arg(dir);
    cmd.current_dir(&temp);
    win::spawn_detached_cmd(&mut cmd).map_err(|e| format!("starting the cleanup: {e}"))?;
    Ok(())
}

/// Remove the install folder, then tidy up the scratch folder this pass is
/// running from.
fn clean_up(dir: Option<&Path>, from_temp: bool) {
    if let Some(dir) = dir {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => report(&format!("Removed {}", dir.display())),
            Err(e) => {
                // The usual reason is that something is still holding a file.
                complain(&format!("Couldn't remove everything in {}: {e}", dir.display()));
                // Nothing else can be done from here; what is left will go on a
                // second run once the file is free.
                std::thread::sleep(Duration::from_millis(500));
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
    if from_temp {
        // Tidy up our own scratch folder on the way out. This program's own
        // image is still open, so it is renamed first — a folder holding only
        // `…exe.old` is one Windows' temporary-file cleanup removes by itself,
        // and nothing here has to lap itself.
        if let Ok(me) = std::env::current_exe() {
            if let Some(scratch) = me.parent() {
                if let Err(e) = move_self_aside(&me) {
                    // Not fatal: the folder still goes, and one leftover file in
                    // the temporary folder is a smaller problem than refusing to
                    // uninstall. Say so rather than hide it.
                    complain(&e);
                }
                if std::fs::remove_dir_all(scratch).is_ok() {
                    report("Cleaned up");
                }
            }
        }
    }
}

// ── small helpers ───────────────────────────────────────────────────────────

fn wait_for_exit(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !win::process_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Both halves of the "where do the shortcuts live" question come from
/// `nv_core::win`, so the installer and the uninstaller cannot disagree about
/// where they were written.
fn start_menu_dir() -> PathBuf {
    win::start_menu_dir()
}

fn desktop_link() -> PathBuf {
    win::desktop_link()
}

/// Progress, for a human running it by hand. There is no window to write to.
fn report(line: &str) {
    let _ = std::io::Write::write_all(&mut std::io::stdout(), format!("{line}\n").as_bytes());
}

fn complain(line: &str) {
    let _ = std::io::Write::write_all(&mut std::io::stderr(), format!("{line}\n").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        Args::parse(list.iter().map(|s| s.to_string()))
    }

    /// Every spelling that ends up in an `UninstallString` has to work: the
    /// shortcut uses `--uninstall`, some launchers rewrite it to `/uninstall`.
    #[test]
    fn every_uninstall_spelling_is_accepted() {
        for spelling in ["--uninstall", "/uninstall", "-uninstall", "--remove", "--quiet", "/S", "--silent"] {
            let a = args(&[spelling]);
            assert!(a.dir.is_none() && !a.delete_settings, "{spelling} was misread");
        }
    }

    #[test]
    fn switches_read_their_values() {
        let a = args(&["--uninstall", "--delete-settings", "--dir", r"C:\Some\Where"]);
        assert!(a.delete_settings);
        assert_eq!(a.dir.as_deref(), Some(Path::new(r"C:\Some\Where")));
        let b = args(&["--parent", "4321", "--dir", "D:\\x"]);
        assert_eq!(b.parent, Some(4321));
        assert_eq!(b.dir.as_deref(), Some(Path::new("D:\\x")));
        // Unknown switches are ignored rather than fatal — Add/Remove Programs
        // has been known to append its own.
        assert!(!args(&["--Windows-Update-Fix"]).delete_settings);
    }

    /// The uninstaller must never treat a folder it cannot identify as ours.
    #[test]
    fn an_unrecognised_folder_is_not_ours() {
        let scratch = std::env::temp_dir().join(format!("nv-uninst-test-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let a = Args { dir: Some(scratch.clone()), delete_settings: false, parent: None, help: false };
        assert_eq!(resolve_dir(&a).unwrap(), scratch, "an explicit --dir is always honoured");
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
