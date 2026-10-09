//! In-app update checks (see AGENTS.md "In-app updates").
//!
//! Both the version check and the upgrade reuse PowerShell instead of an
//! HTTP/zip dependency tree: the check is one `Invoke-RestMethod` against
//! the GitHub API, and "Update & restart" simply runs the official
//! `tools/install.ps1` (the documented one-liner) with `-Relaunch` — the
//! same battle-tested path a manual upgrade takes. Failures are silent:
//! a check that cannot reach the network just never shows a notice.

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

/// Run the official installer (downloads the latest release, closes this
/// running Pound, replaces the exe, re-registers) and relaunch it after.
/// The spawned PowerShell outlives this process; call right before exit.
#[cfg(windows)]
pub fn run_installer() {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Download-then-invoke so the script can take the -Relaunch parameter
    // (`irm … | iex` cannot pass parameters).
    let script = format!(
        "irm https://raw.githubusercontent.com/{REPO}/main/tools/install.ps1 \
         -OutFile \"$env:TEMP\\pound-update.ps1\"; \
         & \"$env:TEMP\\pound-update.ps1\" -Relaunch"
    );
    let spawned = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    if let Err(e) = spawned {
        eprintln!("pound: could not start the updater: {e}");
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
}
