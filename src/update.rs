//! Version check + in-place self-update from GitHub Releases.
//!
//! Mirrors `web/public/install.sh` / `install.ps1`: same repo, archive naming, and
//! `.sha256` sidecars. Checksums are computed in-process with `sha2`.
//!
//! Unix can rename over the running executable. Windows cannot replace a mapped
//! image, so updates rename `honya.exe` aside and reap the `.old` sidecar at startup.
//!
//! Two release channels: `Stable` installs prebuilt release binaries; `Dev`
//! tracks the latest `main` commit and builds it from source on this machine
//! (requires a local Rust toolchain). Dev builds bake their source commit in via
//! the `HONYA_BUILD_COMMIT` env var, which is how a binary knows whether it is a
//! dev build and which commit it was built from.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::model::{AppEvent, EventTx, LogLevel, ReleaseChannel, UpdateMode};

const REPO: &str = "altqx/honya";

/// This build's version, baked in from Cargo.toml at compile time.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The `main` commit this binary was built from, when it is a dev-channel build
/// (the updater bakes it in via `HONYA_BUILD_COMMIT` at `cargo build` time).
/// `None` for release binaries and ordinary local builds.
pub fn built_commit() -> Option<&'static str> {
    option_env!("HONYA_BUILD_COMMIT")
}

/// Human version string: `0.2.2` for stable builds, `0.2.2 (dev abc1234)` for
/// dev-channel builds.
pub fn version_string() -> String {
    match built_commit() {
        Some(sha) => format!("{} (dev {})", current_version(), short_sha(sha)),
        None => current_version().to_string(),
    }
}

/// First 7 hex chars of a commit sha (what `git log --oneline` shows).
fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
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
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

/// The release archive extension for this platform.
const fn archive_ext() -> &'static str {
    if cfg!(windows) { ".zip" } else { ".tar.gz" }
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

/// Query the GitHub API for the sha of the latest commit on `main`.
async fn latest_main_commit() -> Result<Option<String>> {
    let url = format!("https://api.github.com/repos/{REPO}/commits/main");
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
        return Ok(None);
    }
    if !status.is_success() {
        bail!("GitHub API returned {status} for {REPO} (you may be rate-limited — retry shortly)");
    }
    let json: serde_json::Value = resp.json().await?;
    Ok(json
        .get("sha")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

/// The `main` commit a dev-channel update should move to, or `None` when this
/// binary was already built from the tip of `main` (or the check failed). A
/// stable binary on the dev channel always has an "update" (the first dev build).
async fn dev_update_commit() -> Option<String> {
    let sha = latest_main_commit().await.ok().flatten()?;
    if built_commit() == Some(sha.as_str()) {
        None
    } else {
        Some(sha)
    }
}

/// True when an update should install even though the version is not strictly
/// newer: a dev build sitting on the stable channel switches back to the latest
/// release regardless of its version.
fn stable_wants(tag: &str) -> bool {
    is_newer(tag, current_version()) || built_commit().is_some()
}

/// Best-effort startup check. Returns a display version (`0.2.3` or
/// `dev abc1234`) when an update is available, else `None`. Never errors — a
/// failed/blocked network, an unknown platform, or `HONYA_NO_UPDATE_CHECK` all
/// yield `None`.
pub async fn check_for_update(channel: ReleaseChannel) -> Option<String> {
    if std::env::var_os("HONYA_NO_UPDATE_CHECK").is_some() {
        return None;
    }
    match channel {
        ReleaseChannel::Stable => {
            target_triple()?; // we only ship binaries for known platforms
            let tag = latest_release_tag().await.ok().flatten()?;
            stable_wants(&tag).then(|| tag.trim_start_matches('v').to_string())
        }
        ReleaseChannel::Dev => {
            let sha = dev_update_commit().await?;
            Some(format!("dev {}", short_sha(&sha)))
        }
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
async fn auto_update_stable() -> AutoUpdate {
    let Some(target) = target_triple() else {
        return AutoUpdate::UpToDate; // no prebuilt binary for this platform
    };
    let Some(tag) = latest_release_tag().await.ok().flatten() else {
        return AutoUpdate::UpToDate; // unreachable / no published release
    };
    if !stable_wants(&tag) {
        return AutoUpdate::UpToDate;
    }
    let version = tag.trim_start_matches('v').to_string();
    match install_release(&tag, target).await {
        Ok(()) => AutoUpdate::Installed(version),
        Err(_) => AutoUpdate::Available(version),
    }
}

/// Spawn the best-effort background update task: the startup check/auto-install
/// and the re-check after the Settings channel toggle both funnel through here.
/// Reports back over `tx` only; never blocks the UI and never errors.
pub fn spawn_background_update(tx: EventTx, mode: UpdateMode, channel: ReleaseChannel) {
    tokio::spawn(async move {
        if std::env::var_os("HONYA_NO_UPDATE_CHECK").is_some() {
            return;
        }
        match (channel, mode) {
            (ReleaseChannel::Stable, UpdateMode::Auto) => match auto_update_stable().await {
                AutoUpdate::Installed(version) => {
                    tx.send(AppEvent::UpdateInstalled { version });
                }
                // Found but couldn't install (e.g. read-only install dir);
                // fall back to notifying so `honya update` is still offered.
                AutoUpdate::Available(version) => {
                    tx.send(AppEvent::UpdateAvailable { version });
                }
                AutoUpdate::UpToDate => {}
            },
            (ReleaseChannel::Dev, UpdateMode::Auto) => {
                let Some(sha) = dev_update_commit().await else {
                    return;
                };
                let version = format!("dev {}", short_sha(&sha));
                tx.send(AppEvent::Log {
                    level: LogLevel::Info,
                    msg: format!("building honya {version} from source in the background…"),
                });
                match install_dev_build(&sha).await {
                    Ok(()) => tx.send(AppEvent::UpdateInstalled { version }),
                    Err(e) => {
                        tx.send(AppEvent::Log {
                            level: LogLevel::Warn,
                            msg: format!("dev build failed: {e:#}"),
                        });
                        tx.send(AppEvent::UpdateAvailable { version });
                    }
                }
            }
            (_, UpdateMode::Notify) => {
                if let Some(version) = check_for_update(channel).await {
                    tx.send(AppEvent::UpdateAvailable { version });
                }
            }
        }
    });
}

/// `honya update`: bring this install up to date on `channel` — download the
/// latest release (stable) or build the latest `main` commit from source (dev) —
/// and replace the running executable in place. Prints progress to stdout;
/// returns an error with actionable guidance on failure.
pub async fn run_self_update(channel: ReleaseChannel) -> Result<()> {
    match channel {
        ReleaseChannel::Stable => run_self_update_stable().await,
        ReleaseChannel::Dev => run_self_update_dev().await,
    }
}

async fn run_self_update_stable() -> Result<()> {
    let current = current_version();
    println!("honya {} — checking for updates…", version_string());

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

    if !stable_wants(&tag) {
        println!("Already up to date (honya {current}).");
        return Ok(());
    }
    println!("Updating honya {} → {tag} …", version_string());

    install_release(&tag, target).await?;

    println!("✓ honya is now {tag}. Restart it to use the new version.");
    Ok(())
}

async fn run_self_update_dev() -> Result<()> {
    println!(
        "honya {} (dev channel) — checking the latest git commit…",
        version_string()
    );

    let sha = latest_main_commit()
        .await
        .context("could not reach the GitHub API")?
        .ok_or_else(|| anyhow!("could not resolve the main branch of {REPO}"))?;

    if built_commit() == Some(sha.as_str()) {
        println!("Already up to date (dev {}).", short_sha(&sha));
        return Ok(());
    }
    println!(
        "Building honya dev {} from source — this can take a few minutes…",
        short_sha(&sha)
    );

    install_dev_build(&sha).await?;

    println!(
        "✓ honya is now dev {}. Restart it to use the new version.",
        short_sha(&sha)
    );
    Ok(())
}

/// Download the source of `main` at `sha`, build it with the local Rust
/// toolchain, and replace the running executable with the result. Writes nothing
/// to stdout (safe inside the TUI); cargo output is captured, not inherited.
async fn install_dev_build(sha: &str) -> Result<()> {
    let cargo_ok = tokio::process::Command::new("cargo")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !cargo_ok {
        bail!(
            "building from source needs the Rust toolchain (cargo not found) — \
             install it from https://rustup.rs or switch back to the stable channel"
        );
    }

    let tmp = private_staging_dir(short_sha(sha))?;
    let guard = TempDir(tmp.clone());

    // codeload serves the repo snapshot at the exact commit; same archive kind
    // as releases per platform so `extract_archive` is reused as-is.
    let kind = if cfg!(windows) { "zip" } else { "tar.gz" };
    let archive_path = tmp.join(format!("src{}", archive_ext()));
    download_to_file(
        &format!("https://codeload.github.com/{REPO}/{kind}/{sha}"),
        &archive_path,
    )
    .await
    .context("downloading the source archive")?;
    extract_archive(&archive_path, &tmp)?;

    // The snapshot extracts to a single `honya-<sha>/` root.
    let src_root = find_cargo_root(&tmp)
        .ok_or_else(|| anyhow!("the source archive did not contain a Cargo.toml"))?;

    let target_dir = dev_build_target_dir()?;
    let out = tokio::process::Command::new("cargo")
        .args(["build", "--release", "--locked"])
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("HONYA_BUILD_COMMIT", sha)
        .current_dir(&src_root)
        .output()
        .await
        .context("running `cargo build`")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(12).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("`cargo build --release` failed:\n{}", tail.join("\n"));
    }

    let new_bin = target_dir
        .join("release")
        .join(format!("honya{}", std::env::consts::EXE_SUFFIX));
    if !new_bin.is_file() {
        bail!("the build succeeded but produced no honya binary");
    }

    let current_exe = std::env::current_exe().context("resolving the current executable path")?;
    replace_executable(&new_bin, &current_exe)?;
    drop(guard);
    Ok(())
}

/// Locate the directory holding the extracted source's Cargo.toml (one level of
/// nesting, matching codeload's `<repo>-<sha>/` layout).
fn find_cargo_root(root: &Path) -> Option<std::path::PathBuf> {
    if root.join("Cargo.toml").is_file() {
        return Some(root.to_path_buf());
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("Cargo.toml").is_file() {
            return Some(path);
        }
    }
    None
}

/// Download the release for `target` at `tag`, verify its checksum, and replace
/// the running executable in place. The quiet core shared by the `honya update`
/// subcommand and the background [`auto_update`] — it writes nothing to stdout,
/// so it is safe to call from inside the TUI.
async fn install_release(tag: &str, target: &str) -> Result<()> {
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let archive = format!("honya-{target}{}", archive_ext());

    // Private 0700 staging prevents local file swaps mid-update.
    let tmp = private_staging_dir(tag)?;
    let guard = TempDir(tmp.clone());

    let archive_path = tmp.join(&archive);
    download_to_file(&format!("{base}/{archive}"), &archive_path)
        .await
        .with_context(|| format!("downloading {archive}"))?;

    // Fail closed: checksum assets have no archive suffix.
    let sumfile = download_text(&format!("{base}/honya-{target}.sha256"))
        .await
        .with_context(|| {
            format!("could not fetch the checksum for {archive}; refusing to install an unverified binary")
        })?;
    verify_sha256(&archive_path, &sumfile)?;

    extract_archive(&archive_path, &tmp)?;
    let new_bin = find_honya_binary(&tmp)
        .ok_or_else(|| anyhow!("the downloaded archive did not contain a `honya` binary"))?;

    let current_exe = std::env::current_exe().context("resolving the current executable path")?;
    replace_executable(&new_bin, &current_exe)?;
    drop(guard);
    Ok(())
}

/// Extract the downloaded release archive into `dest_dir`.
#[cfg(windows)]
fn extract_archive(archive: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("opening {} to extract", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("reading the downloaded zip archive")?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // `enclosed_name` rejects absolute and `..`-escaping paths.
        let Some(rel) = entry.enclosed_name() else {
            bail!("the downloaded archive contained an unsafe path entry");
        };
        let out_path = dest_dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)
            .with_context(|| format!("writing {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out).context("extracting an archive entry")?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn extract_archive(archive: &Path, dest_dir: &Path) -> Result<()> {
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest_dir)
        .status()
        .context("running `tar` to extract the archive (is tar installed?)")?;
    if !status.success() {
        bail!("`tar` failed to extract {}", archive.display());
    }
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

/// Compute a file's sha256 in-process as lower hex.
fn sha256_hex(file: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(file)
        .with_context(|| format!("opening {} to checksum", file.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf).context("reading the archive to checksum")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for &byte in digest.iter() {
        out.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        out.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    Ok(out)
}

/// Replace `current_exe` with `new_bin`, staging in the same dir to avoid cross-device renames.
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

    #[cfg(not(windows))]
    {
        std::fs::rename(&staged, current_exe).map_err(|e| {
            let _ = std::fs::remove_file(&staged);
            anyhow!(
                "could not replace {}: {e}\n\
                 If honya is installed system-wide, re-run with sudo, or reinstall:\n  \
                 curl -fsSL https://honya.altqx.com/install.sh | bash",
                current_exe.display(),
            )
        })?;
    }

    #[cfg(windows)]
    {
        let old = old_exe_path(current_exe);
        // Free the rename-aside slot from any prior update.
        let _ = std::fs::remove_file(&old);
        std::fs::rename(current_exe, &old).map_err(|e| {
            let _ = std::fs::remove_file(&staged);
            anyhow!(
                "could not move the running {} aside: {e}\n\
                 Re-run the installer to upgrade:\n  \
                 irm https://honya.altqx.com/install.ps1 | iex",
                current_exe.display(),
            )
        })?;
        if let Err(e) = std::fs::rename(&staged, current_exe) {
            // Roll back: restore the original exe and drop the staged copy.
            let _ = std::fs::rename(&old, current_exe);
            let _ = std::fs::remove_file(&staged);
            return Err(anyhow!(
                "could not install the new {}: {e}\n\
                 Re-run the installer to upgrade:\n  \
                 irm https://honya.altqx.com/install.ps1 | iex",
                current_exe.display(),
            ));
        }
        // The old image is still mapped, so this often waits until the next launch.
        let _ = std::fs::remove_file(&old);
    }

    Ok(())
}

/// The `<exe>.old` sidecar path used by the Windows rename-aside swap.
#[cfg(windows)]
fn old_exe_path(current_exe: &Path) -> std::path::PathBuf {
    let mut name = current_exe.as_os_str().to_os_string();
    name.push(".old");
    std::path::PathBuf::from(name)
}

/// Reap the `honya.exe.old` left behind by a previous Windows self-update.
#[cfg(windows)]
pub fn cleanup_stale_old_exe() {
    if let Ok(current_exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_exe_path(&current_exe));
    }
}

#[cfg(not(windows))]
pub fn cleanup_stale_old_exe() {}

/// Private 0700 staging dir; fails closed if a candidate exists.
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
        #[cfg_attr(not(unix), allow(unused_mut))]
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

fn dev_build_target_dir() -> Result<std::path::PathBuf> {
    let dir = user_cache_dir().join("dev-update-target");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating the dev build cache at {}", dir.display()))?;
    Ok(dir)
}

fn user_cache_dir() -> std::path::PathBuf {
    if let Some(xdg) = env_path("XDG_CACHE_HOME") {
        return xdg.join("honya");
    }
    #[cfg(windows)]
    {
        if let Some(local) = env_path("LOCALAPPDATA") {
            return local.join("honya").join("Cache");
        }
        if let Some(appdata) = env_path("APPDATA") {
            return appdata.join("honya").join("Cache");
        }
    }
    if let Some(home) = env_path("HOME") {
        return home.join(".cache").join("honya");
    }
    std::env::temp_dir().join("honya-cache")
}

fn env_path(key: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(std::env::var_os(key)?);
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Locate the honya binary inside the extracted tree, including nested archive layouts.
fn find_honya_binary(root: &Path) -> Option<std::path::PathBuf> {
    let bin_name = format!("honya{}", std::env::consts::EXE_SUFFIX);
    let direct = root.join(&bin_name);
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
            } else if path.file_name().and_then(|n| n.to_str()) == Some(bin_name.as_str()) {
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
}
