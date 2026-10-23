//! Self-update from the GitHub releases of Glasir: the newest release's asset for this
//! platform is downloaded by the app itself (so macOS never quarantines it and Gatekeeper
//! doesn't ask again), unpacked with the OS's own `ditto` / `tar`, and swapped in place.

use crate::git::Res;
use std::path::{Path, PathBuf};

const LATEST: &str = "https://api.github.com/repos/cchvuth/Glasir/releases/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// This platform's download, when the release has one.
    pub asset: Option<String>,
}

/// This platform's release asset and the file it unpacks to.
fn asset_name() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(("Glasir-macos-arm64.zip", "Glasir.app")),
        ("linux", "x86_64") => Some(("Glasir-linux-x86_64.tar.gz", "Glasir")),
        ("windows", "x86_64") => Some(("Glasir-windows-x86_64.zip", "Glasir.exe")),
        _ => None,
    }
}

/// `latest` (a tag like `v1.3.0`) is a higher version than `current`.
fn newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(latest) > parse(current)
}

fn parse(json: &str, current: &str) -> Res<Option<Release>> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("Unexpected reply from GitHub: no release tag")?;
    if !newer(tag, current) {
        return Ok(None);
    }
    let want = asset_name().map(|a| a.0);
    let asset = v["assets"].as_array().into_iter().flatten().find(|a| want.is_some() && a["name"].as_str() == want).and_then(|a| a["browser_download_url"].as_str()).map(str::to_owned);
    Ok(Some(Release { version: tag.trim_start_matches('v').to_owned(), page: v["html_url"].as_str().unwrap_or_default().to_owned(), asset }))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().tls_config(crate::ai::tls()).timeout_connect(Some(std::time::Duration::from_secs(20))).user_agent(concat!("Glasir/", env!("CARGO_PKG_VERSION"))).build().into()
}

/// The newest release, when it is newer than this build. Also clears a finished update's leftovers.
pub fn check() -> Res<Option<Release>> {
    if let Some(t) = target() {
        let _ = std::fs::remove_dir_all(stage(&t));
    }
    let mut r = agent().get(LATEST).header("Accept", "application/vnd.github+json").call().map_err(|e| format!("Could not check for updates: {e}"))?;
    parse(&r.body_mut().read_to_string().map_err(|e| e.to_string())?, env!("CARGO_PKG_VERSION"))
}

/// What an update replaces: the running `.app` bundle on macOS, else the executable.
/// None for a development build (run from a cargo `target` folder).
pub fn target() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    if exe.components().any(|c| c.as_os_str() == "target") {
        return None;
    }
    match cfg!(target_os = "macos") {
        true => exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf),
        false => Some(exe),
    }
}

/// Next to the target, so the swap is a rename on one volume.
fn stage(target: &Path) -> PathBuf {
    target.with_file_name(".glasir-update")
}

fn unpack(archive: &Path, into: &Path) -> Res<()> {
    let (prog, args): (&str, Vec<&std::ffi::OsStr>) = match cfg!(target_os = "macos") {
        true => ("ditto", vec!["-x".as_ref(), "-k".as_ref(), archive.as_os_str(), into.as_os_str()]),
        false => ("tar", vec!["-xf".as_ref(), archive.as_os_str(), "-C".as_ref(), into.as_os_str()]),
    };
    let mut c = std::process::Command::new(prog);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000);
    let out = crate::git::spawn(&mut c).and_then(|c| c.wait_with_output()).map_err(|e| format!("Could not run {prog}: {e}"))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!("Could not unpack the update: {}", String::from_utf8_lossy(&out.stderr).trim())),
    }
}

/// Downloads and unpacks `r`'s asset, then swaps it in for the running copy; returns what to relaunch.
pub fn install(r: &Release) -> Res<PathBuf> {
    install_to(r, target().ok_or("This copy of Glasir can't update itself (a development build).")?)
}

fn install_to(r: &Release, target: PathBuf) -> Res<PathBuf> {
    let (url, (_, inner)) = r.asset.as_deref().zip(asset_name()).ok_or("That release has no download for this platform.")?;
    let stage = stage(&target);
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(stage.join("new")).map_err(|e| format!("Can't write next to {}: {e}", target.display()))?;
    let res = (|| {
        let file = stage.join("download");
        let mut body = agent().get(url).call().map_err(|e| format!("Download failed: {e}"))?.into_body().into_reader();
        let mut out = std::fs::File::create(&file).map_err(|e| e.to_string())?;
        std::io::copy(&mut body, &mut out).map_err(|e| format!("Download failed: {e}"))?;
        drop(out);
        unpack(&file, &stage.join("new"))?;
        swap(&target, &stage.join("new").join(inner), &stage.join("old"))
    })();
    // A running Windows exe can't be deleted: the next check() clears it.
    let _ = std::fs::remove_dir_all(&stage);
    res.map(|_| target)
}

/// Puts `new` where `target` is, keeping `target` at `old` until that worked.
fn swap(target: &Path, new: &Path, old: &Path) -> Res<()> {
    if !new.exists() {
        return Err(format!("The download has no {}.", new.file_name().unwrap_or_default().to_string_lossy()));
    }
    std::fs::rename(target, old).map_err(|e| format!("Can't replace {}: {e}", target.display()))?;
    std::fs::rename(new, target).map_err(|e| {
        let _ = std::fs::rename(old, target);
        format!("Can't replace {}: {e}", target.display())
    })
}

/// Starts `target` once this process has exited (the caller then closes the window).
pub fn relaunch(target: &Path) -> Res<()> {
    let pid = std::process::id().to_string();
    #[cfg(windows)]
    let mut c = {
        let mut c = std::process::Command::new("powershell");
        let path = target.to_string_lossy().replace('\'', "''");
        c.args(["-NoProfile", "-Command", &format!("Wait-Process -Id {pid} -ErrorAction SilentlyContinue; Start-Process -FilePath '{path}'")]);
        std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000);
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let start = if cfg!(target_os = "macos") { "open \"$2\"" } else { "\"$2\" >/dev/null 2>&1 &" };
        let mut c = std::process::Command::new("sh");
        c.args(["-c", &format!("while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; {start}"), "sh", &pid]).arg(target);
        c
    };
    c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    crate::git::spawn(&mut c).map(drop).map_err(|e| format!("Could not restart Glasir: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("v1.10.0", "1.9.3"));
        assert!(newer("v2.0.0", "1.2.0"));
        assert!(newer("v1.2.1", "1.2"));
        assert!(!newer("v1.2.0", "1.2.0"));
        assert!(!newer("v1.1.9", "1.2.0"));
    }

    #[test]
    fn a_release_reply_gives_this_platforms_download() {
        let json = r#"{"tag_name":"v9.0.0","html_url":"https://github.com/cchvuth/Glasir/releases/tag/v9.0.0","assets":[
            {"name":"Glasir-macos-arm64.zip","browser_download_url":"https://x/mac.zip"},
            {"name":"Glasir-linux-x86_64.tar.gz","browser_download_url":"https://x/linux.tgz"},
            {"name":"Glasir-windows-x86_64.zip","browser_download_url":"https://x/win.zip"}]}"#;
        let r = parse(json, "1.2.0").unwrap().unwrap();
        assert_eq!((r.version.as_str(), r.page.as_str()), ("9.0.0", "https://github.com/cchvuth/Glasir/releases/tag/v9.0.0"));
        let want = asset_name().map(|(n, _)| match n {
            "Glasir-macos-arm64.zip" => "https://x/mac.zip",
            "Glasir-linux-x86_64.tar.gz" => "https://x/linux.tgz",
            _ => "https://x/win.zip",
        });
        assert_eq!(r.asset.as_deref(), want);
        assert_eq!(parse(json, "9.0.0").unwrap(), None);
        assert!(parse("{}", "1.0.0").is_err());
    }

    #[test]
    fn a_swap_keeps_the_old_copy_when_the_new_one_is_missing() {
        let dir = std::env::temp_dir().join(format!("glasir-update-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("new")).unwrap();
        let (target, new, old) = (dir.join("Glasir"), dir.join("new/Glasir"), dir.join("old"));
        std::fs::write(&target, "1").unwrap();
        assert!(swap(&target, &new, &old).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "1");
        std::fs::write(&new, "2").unwrap();
        swap(&target, &new, &old).unwrap();
        assert_eq!((std::fs::read_to_string(&target).unwrap(), std::fs::read_to_string(&old).unwrap()), ("2".into(), "1".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Downloads the real latest release: `cargo test -- --ignored installs_the_latest_release`.
    #[test]
    #[ignore]
    fn installs_the_latest_release() {
        let r = parse(&agent().get(LATEST).call().unwrap().body_mut().read_to_string().unwrap(), "0.0.0").unwrap().unwrap();
        let dir = std::env::temp_dir().join(format!("glasir-update-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join(asset_name().unwrap().1);
        std::fs::create_dir_all(&target).unwrap();
        install_to(&r, target.clone()).unwrap();
        assert!(!stage(&target).exists());
        if cfg!(target_os = "macos") {
            assert!(target.join("Contents/MacOS/Glasir").exists());
            let q = std::process::Command::new("xattr").args(["-r", "-p", "com.apple.quarantine"]).arg(&target).output().unwrap();
            assert!(q.stdout.is_empty(), "quarantined: {}", String::from_utf8_lossy(&q.stdout));
            assert!(std::process::Command::new("codesign").args(["--verify", "--deep"]).arg(&target).status().unwrap().success());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_zipped_app_unpacks_with_ditto() {
        let dir = std::env::temp_dir().join(format!("glasir-update-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Glasir.app/Contents")).unwrap();
        std::fs::write(dir.join("Glasir.app/Contents/Info.plist"), "x").unwrap();
        let st = std::process::Command::new("ditto").args(["-c", "-k", "--keepParent"]).arg(dir.join("Glasir.app")).arg(dir.join("a.zip")).status().unwrap();
        assert!(st.success());
        std::fs::create_dir(dir.join("out")).unwrap();
        unpack(&dir.join("a.zip"), &dir.join("out")).unwrap();
        assert!(dir.join("out/Glasir.app/Contents/Info.plist").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
