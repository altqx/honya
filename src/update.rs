//! Version check + in-place self-update from GitHub Releases.
//!
//! Mirrors `web/public/install.sh`: same repo, `honya-<target>.tar.gz` assets, `.sha256`
//! sidecars, and the system `tar` + sha256 tools (resolved via PATH — run only with a
//! trusted PATH). Atomically renames the new binary over the running exe; on Unix the live
//! process keeps the old inode, so replacing a running binary is safe.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

const REPO: &str = "altqx/honya";

/// This build's version, baked in from Cargo.toml at compile time.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn user_agent() -> String {
    format!("honya/{} (+https://github.com/{REPO})", current_version())
}

/// The release target triple for the platform this binary was built for, or
/// `None` on a platform we don't ship prebuilt binaries for. Mirrors the os/arch
/// mapping in install.sh.
pub fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// Parse a `v1.2.3`/`1.2.3-rc1` tag into (major, minor, patch); pre-release/build metadata dropped.
fn parse_semver(s: &str) -> (u64, u64, u64) {
    let s = s.trim().trim_start_matches('v').trim();
    let core = s.split(['-', '+']).next().unwrap_or(s);
    let mut parts = core
        .split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// True when `remote` is a strictly newer version than `local`.
pub fn is_newer(remote: &str, local: &str) -> bool {
    parse_semver(remote) > parse_semver(local)
}

/// Query the GitHub API for the latest published release tag (`tag_name`).
async fn latest_release_tag() -> Result<Option<String>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    let resp = client
        .get(url)
        .header("User-Agent", user_agent())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None); // no published release yet
    }
    if !status.is_success() {
        bail!("GitHub API returned {status} for {REPO} (you may be rate-limited — retry shortly)");
    }
    let json: serde_json::Value = resp.json().await?;
    Ok(json
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

/// Best-effort startup check. Returns the newer version (without a leading `v`)
/// when an update is available, else `None`. Never errors — a failed/blocked
/// network, an unknown platform, or `HONYA_NO_UPDATE_CHECK` all yield `None`.
pub async fn check_for_update() -> Option<String> {
    if std::env::var_os("HONYA_NO_UPDATE_CHECK").is_some() {
        return None;
    }
    target_triple()?; // we only ship binaries for known platforms
    let tag = latest_release_tag().await.ok().flatten()?;
    if is_newer(&tag, current_version()) {
        Some(tag.trim_start_matches('v').to_string())
    } else {
        None
    }
}

/// Outcome of a background auto-update attempt (see [`auto_update`]).
pub enum AutoUpdate {
    /// The latest release was downloaded, verified, and installed in place; the
    /// new binary is live on the next launch. Carries the version (no `v`).
    Installed(String),
    /// A newer release exists but it could not be installed automatically (e.g.
    /// no write permission for the install dir). Fall back to notifying the user
    /// so a manual `honya update` still works. Carries the version (no `v`).
    Available(String),
    /// Nothing to do: already current, an unknown platform, a blocked/failed
    /// network, or `HONYA_NO_UPDATE_CHECK`.
    UpToDate,
}

/// Best-effort startup auto-update for `UpdateMode::Auto`: check for a newer
/// release and, when one exists, install it in the background. Never panics and
/// never writes to stdout (safe to call from inside the TUI). On any install
/// failure it degrades to [`AutoUpdate::Available`] rather than erroring out.
pub async fn auto_update() -> AutoUpdate {
    if std::env::var_os("HONYA_NO_UPDATE_CHECK").is_some() {
        return AutoUpdate::UpToDate;
    }
    let Some(target) = target_triple() else {
        return AutoUpdate::UpToDate; // no prebuilt binary for this platform
    };
    let Some(tag) = latest_release_tag().await.ok().flatten() else {
        return AutoUpdate::UpToDate; // unreachable / no published release
    };
    if !is_newer(&tag, current_version()) {
        return AutoUpdate::UpToDate;
    }
    let version = tag.trim_start_matches('v').to_string();
    match install_release(&tag, target).await {
        Ok(()) => AutoUpdate::Installed(version),
        Err(_) => AutoUpdate::Available(version),
    }
}

/// `honya update`: download the latest release for this platform, verify its
/// checksum, and replace the running executable in place. Prints progress to
/// stdout; returns an error with actionable guidance on failure.
pub async fn run_self_update() -> Result<()> {
    let current = current_version();
    println!("honya {current} — checking for updates…");

    let target = target_triple().ok_or_else(|| {
        anyhow!(
            "no prebuilt binary for this platform ({} {}); reinstall from source: \
             cargo install --git https://github.com/{REPO} honya",
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    })?;

    let tag = latest_release_tag()
        .await
        .context("could not reach the GitHub releases API")?
        .ok_or_else(|| anyhow!("no published release found for {REPO}"))?;

    if !is_newer(&tag, current) {
        println!("Already up to date (honya {current}).");
        return Ok(());
    }
    println!("Updating honya {current} → {tag} …");

    install_release(&tag, target).await?;

    println!("✓ honya is now {tag}. Restart it to use the new version.");
    Ok(())
}

/// Download the release for `target` at `tag`, verify its checksum, and replace
/// the running executable in place. The quiet core shared by the `honya update`
/// subcommand and the background [`auto_update`] — it writes nothing to stdout,
/// so it is safe to call from inside the TUI.
async fn install_release(tag: &str, target: &str) -> Result<()> {
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let archive = format!("honya-{target}.tar.gz");

    // Private, unpredictable temp dir (0700, exclusive) so a local user can't swap files mid-update.
    let tmp = private_staging_dir(tag)?;
    let guard = TempDir(tmp.clone());

    let tar_path = tmp.join(&archive);
    download_to_file(&format!("{base}/{archive}"), &tar_path)
        .await
        .with_context(|| format!("downloading {archive}"))?;

    // Verify the sha256 sidecar (fail closed). Asset is honya-<target>.sha256, no .tar.gz suffix.
    let sumfile = download_text(&format!("{base}/honya-{target}.sha256"))
        .await
        .with_context(|| {
            format!("could not fetch the checksum for {archive}; refusing to install an unverified binary")
        })?;
    verify_sha256(&tar_path, &sumfile)?;

    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tar_path)
        .arg("-C")
        .arg(&tmp)
        .status()
        .context("running `tar` to extract the archive (is tar installed?)")?;
    if !status.success() {
        bail!("`tar` failed to extract {archive}");
    }
    let new_bin = find_honya_binary(&tmp)
        .ok_or_else(|| anyhow!("the downloaded archive did not contain a `honya` binary"))?;

    let current_exe = std::env::current_exe().context("resolving the current executable path")?;
    replace_executable(&new_bin, &current_exe)?;
    drop(guard);
    Ok(())
}

/// Stream a URL to a file.
async fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;
    let bytes = client
        .get(url)
        .header("User-Agent", user_agent())
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    std::fs::write(dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

async fn download_text(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let text = client
        .get(url)
        .header("User-Agent", user_agent())
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(text)
}

/// Verify `file` against a `<hex>  <name>` checksum sidecar.
fn verify_sha256(file: &Path, sumfile: &str) -> Result<()> {
    let expected = sumfile
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("malformed or empty checksum file — refusing to install an unverified binary");
    }
    let actual = sha256_hex(file)?;
    if actual != expected {
        bail!("checksum mismatch — refusing to install (expected {expected}, got {actual})");
    }
    Ok(())
}

/// Compute a file's sha256 using the same tools install.sh relies on.
fn sha256_hex(file: &Path) -> Result<String> {
    let out = std::process::Command::new("sha256sum")
        .arg(file)
        .output()
        .or_else(|_| {
            std::process::Command::new("shasum")
                .arg("-a")
                .arg("256")
                .arg(file)
                .output()
        })
        .context("no sha256 tool found (need `sha256sum` or `shasum`)")?;
    if !out.status.success() {
        bail!("the sha256 tool exited with an error");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase())
}

/// Atomically replace `current_exe` with `new_bin` (same-dir stage + rename).
fn replace_executable(new_bin: &Path, current_exe: &Path) -> Result<()> {
    let dir = current_exe.parent().unwrap_or_else(|| Path::new("."));
    let staged = dir.join(".honya-update.new");
    std::fs::copy(new_bin, &staged).with_context(|| {
        format!(
            "could not write to {} — you may need write permission (or sudo) for the install dir",
            dir.display(),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .context("setting executable permission on the new binary")?;
    }
    std::fs::rename(&staged, current_exe).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        anyhow!(
            "could not replace {}: {e}\n\
             If honya is installed system-wide, re-run with sudo, or reinstall:\n  \
             curl https://honya.altqx.com/install.sh | bash",
            current_exe.display(),
        )
    })?;
    Ok(())
}

/// Private (0700), unpredictable, exclusively-created staging dir; fails closed if a candidate exists.
fn private_staging_dir(tag: &str) -> Result<std::path::PathBuf> {
    let base = std::env::temp_dir();
    for attempt in 0..32u64 {
        let nonce = {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            t ^ ((std::process::id() as u64) << 32) ^ attempt.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        };
        let cand = base.join(format!("honya-update-{tag}-{nonce:016x}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&cand) {
            Ok(()) => return Ok(cand),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("creating a private temp dir for the download"),
        }
    }
    bail!(
        "could not create a unique staging dir under {}",
        base.display()
    );
}

/// Locate the `honya` binary inside the extracted tree: the archive root first,
/// then any nested match — mirroring install.sh's find-fallback.
fn find_honya_binary(root: &Path) -> Option<std::path::PathBuf> {
    let direct = root.join("honya");
    if direct.is_file() {
        return Some(direct);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("honya") {
                return Some(path);
            }
        }
    }
    None
}

/// Removes a directory tree when dropped (best-effort temp cleanup).
struct TempDir(std::path::PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_ordering() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        // pre-release/build metadata is ignored for the comparison
        assert!(!is_newer("0.1.0-rc1", "0.1.0"));
        assert!(is_newer("0.2.0+build5", "0.1.0"));
    }

    #[test]
    fn target_triple_is_known_on_supported_platforms() {
        // On the CI/build platforms we ship for, this must resolve.
        if matches!(std::env::consts::OS, "linux" | "macos") {
            assert!(target_triple().is_some());
        }
    }
}
