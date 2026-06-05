#!/usr/bin/env bash
#
# honya 本屋 — installer
#
#   curl https://honya.altqx.com/install.sh | bash
#
# Downloads the latest prebuilt honya binary for your platform from the
# altqx/honya GitHub releases, verifies its SHA-256 checksum, and installs it
# into ~/.local/bin (override with HONYA_INSTALL_DIR or --dir). Falls back to
# `cargo install` when no prebuilt asset matches your platform.
#
# Environment:
#   HONYA_VERSION       Pin a release tag (e.g. v0.1.0). Default: latest.
#   HONYA_INSTALL_DIR   Install directory. Default: $HOME/.local/bin.
#   NO_COLOR            Disable ANSI colors when set (any value).
#
# Flags:
#   --version <tag>     Same as HONYA_VERSION.
#   --dir <path>        Same as HONYA_INSTALL_DIR.
#   --source            Force install from source via cargo.
#   --help              Show usage.
#
set -euo pipefail

# ----------------------------------------------------------------------------
# Release convention (must match the release workflow exactly).
# ----------------------------------------------------------------------------
readonly REPO="altqx/honya"
readonly BIN="honya"
readonly API_LATEST="https://api.github.com/repos/${REPO}/releases/latest"
readonly DL_BASE="https://github.com/${REPO}/releases/download"
readonly SOURCE_GIT="https://github.com/altqx/honya"

# ----------------------------------------------------------------------------
# Colors — brand palette (truecolor). Honors NO_COLOR and non-TTY stdout.
#   accent indigo #3A5078 · sage #6A8258 · vermilion #B24A3A · amber #B08A4A
# ----------------------------------------------------------------------------
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_RESET=$'\033[0m'
  C_BOLD=$'\033[1m'
  C_DIM=$'\033[38;2;150;142;130m'    # #968E82 ink-faint
  C_ACCENT=$'\033[38;2;58;80;120m'   # #3A5078 indigo
  C_ACCENT2=$'\033[38;2;108;128;162m' # #6C80A2 indigo-soft
  C_OK=$'\033[38;2;106;130;88m'      # #6A8258 sage
  C_WARN=$'\033[38;2;176;138;74m'    # #B08A4A amber
  C_ERR=$'\033[38;2;178;74;58m'      # #B24A3A vermilion
else
  C_RESET=''
  C_BOLD=''
  C_DIM=''
  C_ACCENT=''
  C_ACCENT2=''
  C_OK=''
  C_WARN=''
  C_ERR=''
fi

banner() {
  printf '%s\n' ""
  printf '%s    ╭───────────────────────────────╮%s\n' "$C_ACCENT" "$C_RESET"
  printf '%s    │%s  %shonya%s  %s本屋%s  ·  installer    %s│%s\n' \
    "$C_ACCENT" "$C_RESET" "${C_BOLD}${C_ACCENT}" "$C_RESET" "$C_ACCENT2" "$C_RESET" "$C_ACCENT" "$C_RESET"
  printf '%s    ╰───────────────────────────────╯%s\n' "$C_ACCENT" "$C_RESET"
  printf '%s\n' ""
}

step()  { printf '%s  ▸%s %s\n' "$C_ACCENT" "$C_RESET" "$*"; }
info()  { printf '%s    %s%s\n' "$C_DIM" "$*" "$C_RESET"; }
ok()    { printf '%s  ✓%s %s\n' "$C_OK" "$C_RESET" "$*"; }
warn()  { printf '%s  !%s %s\n' "$C_WARN" "$C_RESET" "$*" >&2; }
die()   { printf '%s  ✗ %s%s\n' "$C_ERR" "$*" "$C_RESET" >&2; exit 1; }

# ----------------------------------------------------------------------------
# Defaults / args
# ----------------------------------------------------------------------------
VERSION="${HONYA_VERSION:-}"
INSTALL_DIR="${HONYA_INSTALL_DIR:-}"
FROM_SOURCE=0

usage() {
  cat <<EOF
honya installer

Usage:
  curl https://honya.altqx.com/install.sh | bash
  curl https://honya.altqx.com/install.sh | bash -s -- [options]

Options:
  --version <tag>   Install a specific release tag (e.g. v0.1.0).
  --dir <path>      Install directory (default: \$HOME/.local/bin).
  --source          Build and install from source via cargo.
  --help            Show this help and exit.

Environment:
  HONYA_VERSION      Same as --version.
  HONYA_INSTALL_DIR  Same as --dir.
  NO_COLOR           Disable colored output.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --help|-h)
      usage; exit 0 ;;
    --version)
      [ "$#" -ge 2 ] || die "--version requires an argument"
      VERSION="$2"; shift 2 ;;
    --version=*)
      VERSION="${1#*=}"; shift ;;
    --dir)
      [ "$#" -ge 2 ] || die "--dir requires an argument"
      INSTALL_DIR="$2"; shift 2 ;;
    --dir=*)
      INSTALL_DIR="${1#*=}"; shift ;;
    --source)
      FROM_SOURCE=1; shift ;;
    *)
      die "Unknown argument: $1 (try --help)" ;;
  esac
done

[ -n "$INSTALL_DIR" ] || INSTALL_DIR="${HOME}/.local/bin"

# ----------------------------------------------------------------------------
# Temp dir + cleanup trap
# ----------------------------------------------------------------------------
TMPDIR_HONYA=""
cleanup() {
  [ -n "$TMPDIR_HONYA" ] && [ -d "$TMPDIR_HONYA" ] && rm -rf "$TMPDIR_HONYA"
}
trap cleanup EXIT INT TERM

# ----------------------------------------------------------------------------
# Helpers
# ----------------------------------------------------------------------------
have() { command -v "$1" >/dev/null 2>&1; }

# download <url> <dest> — curl preferred, wget fallback.
download() {
  url="$1"; dest="$2"
  if have curl; then
    curl -fsSL --proto '=https' --tlsv1.2 -o "$dest" "$url"
  elif have wget; then
    wget -q -O "$dest" "$url"
  else
    die "Neither curl nor wget is available."
  fi
}

# fetch_stdout <url> — print URL body to stdout.
fetch_stdout() {
  url="$1"
  if have curl; then
    curl -fsSL --proto '=https' --tlsv1.2 "$url"
  elif have wget; then
    wget -q -O - "$url"
  else
    die "Neither curl nor wget is available."
  fi
}

# detect_target — set TARGET to a Rust target triple, or empty if unsupported.
detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux)  os_part="unknown-linux-gnu" ;;
    Darwin) os_part="apple-darwin" ;;
    *)
      PLATFORM_DESC="$os/$arch"
      TARGET=""
      return 0 ;;
  esac

  case "$arch" in
    x86_64|amd64)  arch_part="x86_64" ;;
    aarch64|arm64) arch_part="aarch64" ;;
    *)
      PLATFORM_DESC="$os/$arch"
      TARGET=""
      return 0 ;;
  esac

  PLATFORM_DESC="$os/$arch"
  TARGET="${arch_part}-${os_part}"
}

# resolve_version — fill VERSION from the GitHub "latest" API if unset.
resolve_version() {
  [ -n "$VERSION" ] && { info "Using pinned version: ${VERSION}"; return 0; }
  step "Resolving latest release…"
  body="$(fetch_stdout "$API_LATEST")" \
    || die "Failed to query the GitHub releases API. Set HONYA_VERSION to a tag and retry."
  # Portable parse of: "tag_name": "v0.1.0"
  VERSION="$(printf '%s\n' "$body" \
    | grep -m1 '"tag_name"' \
    | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
  [ -n "$VERSION" ] || die "Could not parse the latest tag_name. Set HONYA_VERSION to a tag and retry."
  info "Latest release: ${VERSION}"
}

# sha256_of <file> — print the hex digest using whatever tool exists.
sha256_of() {
  f="$1"
  if have sha256sum; then
    sha256sum "$f" | awk '{print $1}'
  elif have shasum; then
    shasum -a 256 "$f" | awk '{print $1}'
  else
    die "No sha256sum or shasum found to verify the download."
  fi
}

# ----------------------------------------------------------------------------
# Install from source (cargo)
# ----------------------------------------------------------------------------
install_from_source() {
  have cargo || die "cargo not found. Install Rust from https://rustup.rs and re-run with --source."
  step "Building ${BIN} from source via cargo (this can take a few minutes)…"
  if [ -n "$VERSION" ]; then
    info "Pinning tag ${VERSION}"
    cargo install --git "$SOURCE_GIT" --tag "$VERSION" --locked "$BIN" \
      || cargo install --git "$SOURCE_GIT" --tag "$VERSION" "$BIN" \
      || die "cargo install from source failed."
  else
    cargo install --git "$SOURCE_GIT" --locked "$BIN" \
      || cargo install --git "$SOURCE_GIT" "$BIN" \
      || die "cargo install from source failed."
  fi
  cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
  ok "Installed ${C_BOLD}${BIN}${C_RESET}${C_OK} from source to ${cargo_bin}/${BIN}"
  print_path_help "$cargo_bin"
  final_message ""
  exit 0
}

# ----------------------------------------------------------------------------
# Install from a prebuilt release tarball.
# ----------------------------------------------------------------------------
install_from_release() {
  resolve_version

  tarball="${BIN}-${TARGET}.tar.gz"
  url="${DL_BASE}/${VERSION}/${tarball}"
  sum_url="${url}.sha256"

  TMPDIR_HONYA="$(mktemp -d 2>/dev/null || mktemp -d -t honya)"
  tar_path="${TMPDIR_HONYA}/${tarball}"
  sum_path="${tar_path}.sha256"

  step "Downloading ${C_BOLD}${tarball}${C_RESET}"
  info "${url}"
  download "$url" "$tar_path" \
    || die "Download failed for ${url}. The asset may not exist for ${TARGET}; try --source."

  step "Downloading checksum"
  download "$sum_url" "$sum_path" \
    || die "Checksum download failed for ${sum_url}."

  step "Verifying SHA-256 checksum…"
  expected="$(awk '{print $1}' "$sum_path" | head -n1)"
  [ -n "$expected" ] || die "Checksum file was empty: ${sum_url}"
  actual="$(sha256_of "$tar_path")"
  if [ "$expected" != "$actual" ]; then
    die "Checksum mismatch.
    expected: ${expected}
    actual:   ${actual}"
  fi
  ok "Checksum verified"

  step "Extracting archive…"
  tar -xzf "$tar_path" -C "$TMPDIR_HONYA" \
    || die "Failed to extract ${tarball}."

  # The archive contains a single executable named honya; locate it robustly.
  src_bin="${TMPDIR_HONYA}/${BIN}"
  if [ ! -f "$src_bin" ]; then
    src_bin="$(find "$TMPDIR_HONYA" -type f -name "$BIN" -perm -u+x 2>/dev/null | head -n1)"
    [ -n "$src_bin" ] || src_bin="$(find "$TMPDIR_HONYA" -type f -name "$BIN" 2>/dev/null | head -n1)"
  fi
  if [ -z "$src_bin" ] || [ ! -f "$src_bin" ]; then
    die "Could not find a '${BIN}' binary inside the archive."
  fi

  step "Installing to ${C_BOLD}${INSTALL_DIR}${C_RESET}"
  mkdir -p "$INSTALL_DIR" || die "Could not create install dir: ${INSTALL_DIR}"
  dest="${INSTALL_DIR}/${BIN}"
  install -m 0755 "$src_bin" "$dest" 2>/dev/null || {
    cp -f "$src_bin" "$dest" || die "Failed to copy binary to ${dest}"
    chmod +x "$dest" || die "Failed to chmod +x ${dest}"
  }
  ok "Installed ${C_BOLD}${BIN} ${VERSION}${C_RESET}${C_OK} to ${dest}"

  print_path_help "$INSTALL_DIR"
  final_message "$VERSION"
}

# print_path_help <dir> — warn + show shell-specific PATH lines if dir is not on PATH.
print_path_help() {
  dir="$1"
  case ":${PATH}:" in
    *":${dir}:"*)
      return 0 ;;
  esac
  printf '%s\n' ""
  warn "${dir} is not on your PATH."
  printf '%s    Add it with one of:%s\n' "$C_DIM" "$C_RESET"
  printf '\n'
  # These print verbatim shell commands; the literal $PATH is intentional.
  # shellcheck disable=SC2016
  printf '%s    bash%s  echo '\''export PATH="%s:$PATH"'\'' >> ~/.bashrc\n' "$C_ACCENT2" "$C_RESET" "$dir"
  # shellcheck disable=SC2016
  printf '%s    zsh %s  echo '\''export PATH="%s:$PATH"'\'' >> ~/.zshrc\n'  "$C_ACCENT2" "$C_RESET" "$dir"
  printf '%s    fish%s  fish_add_path %s\n'                                  "$C_ACCENT2" "$C_RESET" "$dir"
  printf '\n'
  printf '%s    Then restart your shell (or source the file above).%s\n' "$C_DIM" "$C_RESET"
}

# final_message <version> — closing success block.
final_message() {
  ver="$1"
  printf '%s\n' ""
  if [ -n "$ver" ]; then
    printf '%s  ✓ honya %s%s%s installed.%s\n' "$C_OK" "$C_BOLD" "$ver" "${C_RESET}${C_OK}" "$C_RESET"
  else
    printf '%s  ✓ honya installed.%s\n' "$C_OK" "$C_RESET"
  fi
  printf '%s    Run:%s %shonya%s\n' "$C_DIM" "$C_RESET" "${C_BOLD}${C_ACCENT}" "$C_RESET"
  printf '%s\n' ""
}

# ----------------------------------------------------------------------------
# Main
# ----------------------------------------------------------------------------
main() {
  banner

  if [ "$FROM_SOURCE" -eq 1 ]; then
    install_from_source
  fi

  detect_target
  if [ -z "$TARGET" ]; then
    warn "No prebuilt honya binary for your platform (${PLATFORM_DESC})."
    if have cargo; then
      info "Falling back to a source build via cargo."
      install_from_source
    fi
    die "Unsupported platform: ${PLATFORM_DESC}.
    Prebuilt binaries cover: x86_64/aarch64 Linux (gnu) and macOS.
    Install Rust (https://rustup.rs) and re-run with --source to build from source:
      curl https://honya.altqx.com/install.sh | bash -s -- --source"
  fi

  info "Platform: ${PLATFORM_DESC} → ${TARGET}"
  install_from_release
}

main "$@"
