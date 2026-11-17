//! Putting a downloaded release in place of the running app (自动下载更新).
//! The package is the one the release page offers (`wisp_core::update::asset`),
//! already held against its checksum by `wisp_net::download_release`.
//! `stage` unpacks it into a folder beside the app, `.wisp-paper-update`, on
//! the same disk, so that `install` is two renames: the app in use steps
//! aside into that folder, the new one takes its place, and it is started
//! once this one has quit. The next start clears the folder (`tidy`).
//!
//! macOS replaces the `.app` bundle from the DMG; Windows the files of the
//! app's folder from the zip (a running program can be renamed, not written
//! over), whether the folder was unpacked by hand or made by the setup, which
//! installs into a folder of the person's own (`scripts/wisp-paper.iss`). Elsewhere, and for a build run from `target/`, there is
//! nothing to replace: the release is only announced.

use std::path::{Path, PathBuf};

/// Whether this platform's package can be installed by the app itself.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "windows"));

const WORK: &str = ".wisp-paper-update";

/// The package of a release that this build would install.
pub fn asset(version: &str) -> Option<String> {
    wisp_core::update::asset(version, std::env::consts::OS, std::env::consts::ARCH)
}

/// What a release would replace: the `.app` bundle on macOS, the folder of
/// `WispPaper.exe` on Windows. `None` when this is not a packaged app, or
/// when it is where nothing can be written (run off its disk image, a folder
/// of another user).
pub fn target() -> Option<PathBuf> {
    let target = imp::place(&std::env::current_exe().ok()?)?;
    std::fs::create_dir_all(imp::work(&target)).ok()?;
    Some(target)
}

/// Unpack a downloaded package beside what it replaces; gives the new app, ready for `install`.
pub fn stage(package: &[u8], target: &Path) -> Result<PathBuf, String> {
    let work = imp::work(target);
    // What an earlier update left is in the way. (The app it set aside may still be open on Windows: then its file stays.)
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(io)?;
    imp::stage(package, target, &work)
}

/// Put the staged app in place of the running one and have it started once this one is gone. The caller quits.
pub fn install(staged: &Path, target: &Path) -> Result<(), String> {
    imp::install(staged, target, &imp::work(target))
}

/// Clear what the last update left beside the app: the app it replaced, or a package never installed.
pub fn tidy() {
    if let Some(target) = std::env::current_exe().ok().and_then(|exe| imp::place(&exe)) {
        #[cfg(target_os = "windows")]
        imp::updated(&target);
        let _ = std::fs::remove_dir_all(imp::work(&target));
    }
}

fn io(err: std::io::Error) -> String {
    err.to_string()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{WORK, io};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The name of the app in the disk image (`scripts/package-macos.sh`).
    const APP: &str = "Wisp Paper.app";

    /// `…/Wisp Paper.app/Contents/MacOS/wisp_paper` is in a bundle; `target/release/wisp_paper` is not.
    pub fn place(exe: &Path) -> Option<PathBuf> {
        let bundle = exe.parent()?.parent()?.parent()?;
        (bundle.extension()? == "app" && exe.parent()?.ends_with("Contents/MacOS")).then(|| bundle.to_path_buf())
    }

    pub fn work(target: &Path) -> PathBuf {
        target.with_file_name(WORK)
    }

    fn run(command: &mut Command) -> Result<String, String> {
        let name = command.get_program().to_string_lossy().to_string();
        let output = command.output().map_err(|err| format!("{name}: {err}"))?;
        if !output.status.success() { return Err(format!("{name}: {}", String::from_utf8_lossy(&output.stderr).trim())) }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn stage(package: &[u8], target: &Path, work: &Path) -> Result<PathBuf, String> {
        let (image, mount, staged) = (work.join("update.dmg"), work.join("image"), work.join(APP));
        std::fs::write(&image, package).map_err(io)?;
        std::fs::create_dir_all(&mount).map_err(io)?;
        run(Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mount).arg(&image))?;
        let copied = run(Command::new("ditto").arg(mount.join(APP)).arg(&staged));
        // Letting go of an image the system is still looking over can take a minute; the copy does not need to wait for it.
        std::thread::spawn(move || {
            let _ = Command::new("hdiutil").args(["detach", "-force"]).arg(&mount).output();
            let _ = std::fs::remove_file(&image);
        });
        copied?;
        verify(&staged, target)?;
        Ok(staged)
    }

    /// The new app must be sealed, and signed by whoever signed this one: it has to meet the
    /// running app's designated requirement (its identifier under the same Developer ID team).
    /// A build signed ad hoc has no such requirement to hold another to; there only the seal counts.
    fn verify(staged: &Path, target: &Path) -> Result<(), String> {
        let mut check = Command::new("codesign");
        check.args(["--verify", "--deep", "--strict"]);
        let own = Command::new("codesign").args(["-d", "-r-"]).arg(target).output().map(|out| String::from_utf8_lossy(&out.stdout).to_string()).unwrap_or_default();
        if let Some(requirement) = own.lines().find_map(|line| line.strip_prefix("designated => ")).filter(|r| r.contains("anchor apple generic")) {
            check.arg("-R").arg(format!("={requirement}"));
        }
        run(check.arg(staged)).map(|_| ()).map_err(|err| tf!("新版本的签名与当前应用不符，没有安装（{err}）", "The new version is not signed like this app and was not installed ({err})"))
    }

    pub fn install(staged: &Path, target: &Path, work: &Path) -> Result<(), String> {
        let old = work.join("old.app");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(target, &old).map_err(io)?;
        if let Err(err) = std::fs::rename(staged, target) {
            let _ = std::fs::rename(&old, target);
            return Err(io(err));
        }
        // `open` on an app that is still running only brings it forward: wait for this one to be gone.
        // An app started by `open` has none of this one's environment: the data folder it was pointed at goes along.
        let _ = Command::new("/bin/sh").args(["-c", r#"while kill -0 "$0" 2>/dev/null; do sleep 0.2; done; open "$1" ${WISP_PAPER_HOME:+--env "WISP_PAPER_HOME=$WISP_PAPER_HOME"}"#]).arg(std::process::id().to_string()).arg(target).spawn();
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{WORK, io, swap};
    use std::path::{Path, PathBuf};

    /// The name the package gives the app (`scripts/package-windows.ps1`); cargo's own is `wisp_paper.exe`.
    const APP: &str = "WispPaper.exe";

    pub fn place(exe: &Path) -> Option<PathBuf> {
        (exe.file_name()?.to_str()?.eq_ignore_ascii_case(APP)).then(|| exe.parent().map(Path::to_path_buf))?
    }

    pub fn work(target: &Path) -> PathBuf {
        target.join(WORK)
    }

    // ponytail: the zip is trusted on its checksum alone (as a download from the release page is); WinVerifyTrust on the exe if that is ever not enough.
    pub fn stage(package: &[u8], _target: &Path, work: &Path) -> Result<PathBuf, String> {
        let staged = work.join("new");
        std::fs::create_dir_all(&staged).map_err(io)?;
        for (name, data) in wisp_core::update::unpack(package)? {
            std::fs::write(staged.join(name), data).map_err(io)?;
        }
        if !staged.join(APP).is_file() { return Err(tf!("更新包里没有 {APP}", "The package holds no {APP}")) }
        Ok(staged)
    }

    /// A copy the setup installed (`scripts/wisp-paper.iss`) is listed among the system's apps with
    /// its version. Right after an update (the app set aside is still in `old`) the entry is made
    /// to say this one's. A portable folder has no entry, and none is made for it.
    pub fn updated(target: &Path) {
        use std::os::windows::process::CommandExt;
        const ENTRY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\WispPaper_is1";
        if !work(target).join("old").is_dir() || !target.join("unins000.exe").is_file() { return }
        // `reg` is a console program: CREATE_NO_WINDOW keeps its window away.
        let reg = |args: &[&str]| std::process::Command::new("reg").args(args).creation_flags(0x0800_0000).output().is_ok_and(|output| output.status.success());
        std::thread::spawn(move || {
            if reg(&["query", ENTRY]) { reg(&["add", ENTRY, "/v", "DisplayVersion", "/d", env!("CARGO_PKG_VERSION"), "/f"]); }
        });
    }

    pub fn install(staged: &Path, target: &Path, work: &Path) -> Result<(), String> {
        swap(staged, target, &work.join("old"))?;
        // The new app must not take this window for an instance to hand over to.
        crate::background::forget();
        let _ = std::process::Command::new(target.join(APP)).spawn();
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use super::WORK;
    use std::path::{Path, PathBuf};

    pub fn place(_exe: &Path) -> Option<PathBuf> {
        None
    }

    pub fn work(target: &Path) -> PathBuf {
        target.join(WORK)
    }

    pub fn stage(_package: &[u8], _target: &Path, _work: &Path) -> Result<PathBuf, String> {
        Err("unsupported".into())
    }

    pub fn install(_staged: &Path, _target: &Path, _work: &Path) -> Result<(), String> {
        Err("unsupported".into())
    }
}

/// Windows: each file of `staged` takes the place of the one of its name in `target`, which is
/// set aside in `old` first. The app's own file goes last: if a file cannot move, the app has not changed.
#[cfg(any(target_os = "windows", test))]
fn swap(staged: &Path, target: &Path, old: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(old);
    std::fs::create_dir_all(old).map_err(io)?;
    let mut names: Vec<_> = std::fs::read_dir(staged).map_err(io)?.filter_map(|entry| Some(entry.ok()?.file_name())).collect();
    names.sort_by_key(|name| name.to_string_lossy().eq_ignore_ascii_case("WispPaper.exe"));
    for name in names {
        let place = target.join(&name);
        if place.exists() { std::fs::rename(&place, old.join(&name)).map_err(io)? }
        std::fs::rename(staged.join(&name), &place).map_err(io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::swap;

    #[test]
    fn new_files_take_the_place_of_the_old_ones() {
        let dir = std::env::temp_dir().join(format!("wisp-updater-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (staged, old) = (dir.join(".wisp-paper-update/new"), dir.join(".wisp-paper-update/old"));
        std::fs::create_dir_all(&staged).unwrap();
        for (name, text) in [("WispPaper.exe", "app 1"), ("wisp.exe", "cli 1"), ("mine.txt", "kept")] { std::fs::write(dir.join(name), text).unwrap() }
        for (name, text) in [("WispPaper.exe", "app 2"), ("wisp.exe", "cli 2"), ("README.md", "new")] { std::fs::write(staged.join(name), text).unwrap() }
        swap(&staged, &dir, &old).unwrap();
        let read = |path: std::path::PathBuf| std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!([read(dir.join("WispPaper.exe")), read(dir.join("wisp.exe")), read(dir.join("README.md")), read(dir.join("mine.txt"))], ["app 2", "cli 2", "new", "kept"]);
        assert_eq!([read(old.join("WispPaper.exe")), read(old.join("wisp.exe"))], ["app 1", "cli 1"]);
        assert!(std::fs::read_dir(&staged).unwrap().next().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
