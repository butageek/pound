//! In-app update checks (see AGENTS.md "In-app updates").
//!
//! Both the version check and the upgrade reuse PowerShell instead of an
//! HTTP/zip dependency tree: the check is one `Invoke-RestMethod` against
//! the GitHub API, and "Update & restart" simply runs the official
//! `tools/install.ps1` (the documented one-liner) with `-Relaunch` — the
//! same battle-tested path a manual upgrade takes. Failures are silent:
//! a check that cannot reach the network just never shows a notice.

use std::path::Path;
use std::sync::Mutex;

/// Compare two versions ("v0.4.1" or "0.4.1"). Anything unparseable
/// (garbage, prerelease suffixes) counts as "not newer" so it can never
/// nag the user into a bogus update.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(version: &str) -> Option<(u64, u64, u64)> {
        let numbers: Vec<&str> = version.trim().trim_start_matches('v').split('.').collect();
        if numbers.len() != 3 {
            return None;
        }
        Some((
            numbers[0].parse().ok()?,
            numbers[1].parse().ok()?,
            numbers[2].parse().ok()?,
        ))
    }
    match (parts(latest), parts(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

#[cfg(windows)]
pub const REPO: &str = "butageek/pound";

/// The latest release tag ("v0.4.1"), or `None` offline/rate-limited.
#[cfg(windows)]
pub fn latest_release_tag() -> Option<String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = format!(
        "(Invoke-RestMethod -Uri 'https://api.github.com/repos/{REPO}/releases/latest' \
         -TimeoutSec 10 -Headers @{{ 'User-Agent' = 'pound' }}).tag_name"
    );
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let tag = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !tag.is_empty()).then_some(tag)
}

/// Build the PowerShell that runs the official installer. `open_path` is
/// reopened by the relaunched Pound; embedded quotes are doubled per
/// PowerShell escaping rules.
#[cfg(windows)]
fn installer_script(open_path: Option<&Path>) -> String {
    let open = open_path
        .map(|path| {
            let escaped = path.display().to_string().replace('"', "\"\"");
            format!(" -OpenPath \"{escaped}\"")
        })
        .unwrap_or_default();
    // Download-then-invoke so the script can take parameters
    // (`irm … | iex` cannot pass parameters).
    format!(
        "irm https://raw.githubusercontent.com/{REPO}/main/tools/install.ps1 \
         -OutFile \"$env:TEMP\\pound-update.ps1\"; \
         & \"$env:TEMP\\pound-update.ps1\" -Relaunch{open}"
    )
}

/// Live state of a running installer, streamed to the shell's toast.
#[derive(Default)]
pub struct InstallerRun {
    /// The installer's own progress lines (Write-Step output).
    pub lines: Vec<String>,
    pub finished: bool,
    pub success: bool,
}

pub type InstallerSlot = Mutex<Option<InstallerRun>>;

/// Spawn the official installer with piped output so the caller can show
/// its progress (`run::read_installer_output` streams the lines). The
/// installer closes this running Pound itself (graceful, then force)
/// before replacing the exe, so the caller stays alive and shows progress
/// until then. `open_path` is reopened by the relaunched Pound.
#[cfg(windows)]
pub fn spawn_installer(open_path: Option<&Path>) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &installer_script(open_path),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::piped())
        .spawn()
}

/// Stream the installer's progress lines into `run` until it exits.
#[cfg(windows)]
pub fn read_installer_output(mut child: std::process::Child, run: &InstallerSlot) {
    use std::io::BufRead;

    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(state) = run.lock().unwrap().as_mut() {
                state.lines.push(line);
            }
        }
    }
    let success = child.wait().map(|status| status.success()).unwrap_or(false);
    if let Some(state) = run.lock().unwrap().as_mut() {
        state.finished = true;
        state.success = success;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_are_detected() {
        assert!(is_newer("v0.5.0", "0.4.9"));
        assert!(is_newer("0.4.2", "v0.4.1"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.10.0", "v0.9.0")); // numeric, not lexicographic
    }

    #[test]
    fn same_older_and_garbage_are_not_newer() {
        assert!(!is_newer("v0.4.0", "0.4.0"));
        assert!(!is_newer("v0.3.9", "0.4.0"));
        assert!(!is_newer("garbage", "0.4.0"));
        assert!(!is_newer("v0.5.0-beta", "0.4.0")); // prerelease: never nag
        assert!(!is_newer("", "0.4.0"));
    }

    #[cfg(windows)]
    #[test]
    fn installer_script_carries_the_open_path() {
        let script = installer_script(Some(Path::new("C:\\dir with space\\a.md")));
        assert!(script.contains("-Relaunch"));
        assert!(script.contains("-OpenPath \"C:\\dir with space\\a.md\""));

        assert!(installer_script(None).contains("-Relaunch"));
        assert!(!installer_script(None).contains("-OpenPath"));

        // Embedded quotes are doubled (PowerShell escaping).
        let quoted = installer_script(Some(Path::new("we\"ird.md")));
        assert!(quoted.contains("-OpenPath \"we\"\"ird.md\""));
    }
}
