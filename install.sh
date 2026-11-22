#!/bin/sh
# Orqella one-command installer for Linux and macOS.
#
#   curl -fsSL https://orqella.io/install.sh | sh
#
# Installs the daemon and CLI, the desktop app, and the per-user service for the
# current user. Release archives are verified against the published SHA-256
# checksums. The installer never uses sudo and does not need Rust, Git, or a
# source checkout.
#
# Options:
#   --version <version>          daemon release to install (default: latest)
#   --desktop-version <version>  desktop release to install (default: latest)
#   --install-dir <dir>          daemon install directory (default: ~/.local/bin)
#   --no-desktop                 skip the desktop app
#   --no-service                 do not install or restart the per-user service
#   --firewall                   try to add the LAN-scoped Link firewall rule
#   --dry-run                    print the plan without downloading
#   -h, --help                   show this help
#
# Environment equivalents:
#   ORQELLA_VERSION, ORQELLA_DESKTOP_VERSION, ORQELLA_INSTALL_DIR,
#   ORQELLA_NO_DESKTOP, ORQELLA_NO_SERVICE, ORQELLA_FIREWALL,
#   ORQELLA_BASE_URL, ORQELLA_GITHUB_REPO
#
# Windows uses the combined installer from https://orqella.io/#download.

set -eu

BASE_URL="${ORQELLA_BASE_URL:-https://orqella.io}"
REPO="${ORQELLA_GITHUB_REPO:-orqella/orqella}"
INSTALL_DIR="${ORQELLA_INSTALL_DIR:-$HOME/.local/bin}"
DAEMON_VERSION="${ORQELLA_VERSION:-}"
DESKTOP_VERSION="${ORQELLA_DESKTOP_VERSION:-}"

if [ -n "${ORQELLA_NO_DESKTOP:-}" ]; then
  INSTALL_DESKTOP=no
else
  INSTALL_DESKTOP=yes
fi
if [ -n "${ORQELLA_NO_SERVICE:-}" ]; then
  INSTALL_SERVICE=no
else
  INSTALL_SERVICE=yes
fi
if [ -n "${ORQELLA_FIREWALL:-}" ]; then
  INSTALL_FIREWALL=yes
else
  INSTALL_FIREWALL=no
fi
DRY_RUN=no

say() { printf '==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Orqella one-command installer for Linux and macOS.

Usage: install.sh [options]

  --version <version>          daemon release to install (default: latest)
  --desktop-version <version>  desktop release to install (default: latest)
  --install-dir <dir>          daemon install directory (default: ~/.local/bin)
  --no-desktop                 skip the desktop app
  --no-service                 do not install or restart the per-user service
  --firewall                   try to add the LAN-scoped Link firewall rule
  --dry-run                    print the plan without downloading
  -h, --help                   show this help

Environment: ORQELLA_VERSION, ORQELLA_DESKTOP_VERSION, ORQELLA_INSTALL_DIR,
ORQELLA_NO_DESKTOP, ORQELLA_NO_SERVICE, ORQELLA_FIREWALL, ORQELLA_BASE_URL,
ORQELLA_GITHUB_REPO.

Windows uses the combined installer from https://orqella.io/#download.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || die "--version needs a value"
      DAEMON_VERSION=$2
      shift 2
      ;;
    --version=*)
      DAEMON_VERSION=${1#*=}
      shift
      ;;
    --desktop-version)
      [ $# -ge 2 ] || die "--desktop-version needs a value"
      DESKTOP_VERSION=$2
      shift 2
      ;;
    --desktop-version=*)
      DESKTOP_VERSION=${1#*=}
      shift
      ;;
    --install-dir)
      [ $# -ge 2 ] || die "--install-dir needs a value"
      INSTALL_DIR=$2
      shift 2
      ;;
    --install-dir=*)
      INSTALL_DIR=${1#*=}
      shift
      ;;
    --no-desktop)
      INSTALL_DESKTOP=no
      shift
      ;;
    --no-service)
      INSTALL_SERVICE=no
      shift
      ;;
    --firewall)
      INSTALL_FIREWALL=yes
      shift
      ;;
    --dry-run)
      DRY_RUN=yes
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1 (use --help)"
      ;;
  esac
done

DAEMON_VERSION=${DAEMON_VERSION#v}
DESKTOP_VERSION=${DESKTOP_VERSION#desktop-v}
DESKTOP_VERSION=${DESKTOP_VERSION#v}

os=$(uname -s 2>/dev/null || echo unknown)
arch=$(uname -m 2>/dev/null || echo unknown)
case "$os" in
  Linux)
    case "$arch" in
      x86_64|amd64)
        DAEMON_PLATFORM=linux-x86_64
        DESKTOP_ARCH=amd64
        ;;
      aarch64|arm64)
        DAEMON_PLATFORM=linux-aarch64
        DESKTOP_ARCH=aarch64
        ;;
      *)
        die "unsupported Linux architecture: $arch"
        ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      arm64|aarch64)
        DAEMON_PLATFORM=macos-arm64
        DESKTOP_ARCH=aarch64
        ;;
      x86_64)
        DAEMON_PLATFORM=macos-x86_64
        DESKTOP_ARCH=x64
        ;;
      *)
        die "unsupported macOS architecture: $arch"
        ;;
    esac
    ;;
  *)
    die "this installer supports Linux and macOS; on Windows use the installer at $BASE_URL/#download"
    ;;
esac

if [ "$os" = Linux ] && [ "$arch" != x86_64 ] && [ "$arch" != amd64 ]; then
  die "prebuilt Linux releases are currently x86_64 only; build from source for $arch"
fi

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/orqella-install.XXXXXX") || die "could not create a temporary directory"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup 0 1 2 15

fetch() {
  url=$1
  output=$2
  if command -v curl >/dev/null 2>&1; then
    curl -fL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 --connect-timeout 15 --progress-bar -o "$output" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -O "$output" "$url"
  else
    die "curl or wget is required"
  fi
}

fetch_text() {
  url=$1
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 2 --connect-timeout 15 "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "$url"
  else
    die "curl or wget is required"
  fi
}

fetch_local() {
  url=$1
  if command -v curl >/dev/null 2>&1; then
    curl -fsS --max-time 2 "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- --timeout=2 "$url"
  else
    return 1
  fi
}

valid_version() {
  case "$1" in
    ''|*[!0-9A-Za-z.-]*) return 1 ;;
    *) return 0 ;;
  esac
}

resolve_versions() {
  versions=
  if [ -z "$DAEMON_VERSION" ] || { [ "$INSTALL_DESKTOP" = yes ] && [ -z "$DESKTOP_VERSION" ]; }; then
    versions=$(fetch_text "$BASE_URL/assets/latest.txt" 2>/dev/null || true)
  fi

  if [ -z "$DAEMON_VERSION" ]; then
    DAEMON_VERSION=$(printf '%s\n' "$versions" | sed -n 's/^daemon_version=//p' | sed -n '1p')
  fi
  if [ "$INSTALL_DESKTOP" = yes ] && [ -z "$DESKTOP_VERSION" ]; then
    DESKTOP_VERSION=$(printf '%s\n' "$versions" | sed -n 's/^desktop_version=//p' | sed -n '1p')
  fi

  if [ -z "$DAEMON_VERSION" ]; then
    latest=$(fetch_text "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null || true)
    DAEMON_VERSION=$(printf '%s\n' "$latest" \
      | sed -n 's/.*"tag_name": *"v\{0,1\}\([0-9][^"]*\)".*/\1/p' \
      | sed -n '1p')
  fi
  if [ -z "$DAEMON_VERSION" ] && command -v curl >/dev/null 2>&1; then
    final_url=$(curl -fsSIL -o /dev/null -w '%{url_effective}' \
      "https://github.com/$REPO/releases/latest" 2>/dev/null || true)
    DAEMON_VERSION=$(printf '%s\n' "$final_url" | sed -n 's#.*/tag/v\{0,1\}\([0-9][^/]*\)$#\1#p')
  fi

  if [ "$INSTALL_DESKTOP" = yes ] && [ -z "$DESKTOP_VERSION" ]; then
    releases=$(fetch_text "https://api.github.com/repos/$REPO/releases?per_page=100" 2>/dev/null || true)
    DESKTOP_VERSION=$(printf '%s\n' "$releases" \
      | grep -o '"tag_name": *"desktop-v[0-9][0-9A-Za-z.-]*"' 2>/dev/null \
      | sed 's/.*desktop-v//;s/"$//' \
      | grep -v -e '-' \
      | sort -t. -k1,1n -k2,2n -k3,3n 2>/dev/null \
      | tail -1)
  fi

  valid_version "$DAEMON_VERSION" || die "could not determine the latest daemon release; pass --version <version>"
  if [ "$INSTALL_DESKTOP" = yes ]; then
    if ! valid_version "$DESKTOP_VERSION"; then
      warn "could not determine a desktop release; continuing with the daemon only"
      INSTALL_DESKTOP=no
    fi
  fi

  if [ "$INSTALL_DESKTOP" = yes ] && [ "$os" = Linux ] && [ "$DESKTOP_ARCH" != amd64 ]; then
    warn "no prebuilt desktop app is published for Linux $arch; continuing with the daemon only"
    INSTALL_DESKTOP=no
  fi
}

resolve_versions

daemon_asset="orqella-${DAEMON_VERSION}-${DAEMON_PLATFORM}.tar.gz"
daemon_url="https://github.com/${REPO}/releases/download/v${DAEMON_VERSION}/${daemon_asset}"
daemon_checksum_url="${daemon_url}.sha256"

if [ "$INSTALL_DESKTOP" = yes ]; then
  if [ "$os" = Darwin ]; then
    desktop_asset="Orqella.Desktop_${DESKTOP_VERSION}_${DESKTOP_ARCH}.app.tar.gz"
  else
    desktop_asset="Orqella.Desktop_${DESKTOP_VERSION}_${DESKTOP_ARCH}.AppImage"
  fi
  desktop_base="https://github.com/${REPO}/releases/download/desktop-v${DESKTOP_VERSION}"
  desktop_url="${desktop_base}/${desktop_asset}"
  desktop_checksums_url="${desktop_base}/SHA256SUMS.txt"
fi

print_plan() {
  printf 'Platform: %s %s\n' "$os" "$arch"
  printf 'Daemon:   %s\n' "$DAEMON_VERSION"
  printf '          %s\n' "$daemon_url"
  if [ "$INSTALL_DESKTOP" = yes ]; then
    printf 'Desktop:  %s\n' "$DESKTOP_VERSION"
    printf '          %s\n' "$desktop_url"
  else
    printf 'Desktop:  skipped\n'
  fi
  printf 'Install:  %s\n' "$INSTALL_DIR"
  if [ "$INSTALL_SERVICE" = yes ]; then
    printf 'Service:  per-user service\n'
  else
    printf 'Service:  skipped\n'
  fi
  if [ "$INSTALL_FIREWALL" = yes ]; then
    printf 'Firewall: ask to add the LAN-scoped Link rule\n'
  else
    printf 'Firewall: skipped (run: %s firewall install)\n' "$INSTALL_DIR/orqella"
  fi
}

if [ "$DRY_RUN" = yes ]; then
  print_plan
  exit 0
fi

if [ ! -d "$INSTALL_DIR" ]; then
  mkdir -p "$INSTALL_DIR" || die "could not create $INSTALL_DIR"
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1; exit}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1; exit}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF; exit}'
  else
    die "no SHA-256 tool found (sha256sum, shasum, or openssl)"
  fi
}

verify_sha256() {
  file=$1
  expected=$2
  [ -n "$expected" ] || die "no checksum is published for $(basename "$file")"
  actual=$(sha256_file "$file")
  if [ "$actual" != "$expected" ]; then
    printf 'expected: %s\nactual:   %s\n' "$expected" "$actual" >&2
    die "checksum mismatch for $(basename "$file")"
  fi
}

say "Downloading Orqella $DAEMON_VERSION for $DAEMON_PLATFORM"
fetch "$daemon_url" "$TMP_DIR/$daemon_asset"
fetch "$daemon_checksum_url" "$TMP_DIR/daemon.sha256"
verify_sha256 "$TMP_DIR/$daemon_asset" "$(awk '{print $1; exit}' "$TMP_DIR/daemon.sha256")"

if [ "$INSTALL_DESKTOP" = yes ]; then
  say "Downloading Orqella Desktop $DESKTOP_VERSION"
  fetch "$desktop_url" "$TMP_DIR/$desktop_asset"
  fetch "$desktop_checksums_url" "$TMP_DIR/desktop-sums.txt"
  expected=$(awk -v name="$desktop_asset" '{n=$NF; sub(/^\*/, "", n); if (n == name) { print $1; exit }}' "$TMP_DIR/desktop-sums.txt")
  verify_sha256 "$TMP_DIR/$desktop_asset" "$expected"
fi

say "Installing the daemon to $INSTALL_DIR"
mkdir -p "$TMP_DIR/extract"
tar -xzf "$TMP_DIR/$daemon_asset" -C "$TMP_DIR/extract" || die "could not extract the daemon archive"
[ -f "$TMP_DIR/extract/orqella" ] || die "the daemon archive did not contain orqella"
cp "$TMP_DIR/extract/orqella" "$INSTALL_DIR/.orqella.$$"
chmod 0755 "$INSTALL_DIR/.orqella.$$"
mv -f "$INSTALL_DIR/.orqella.$$" "$INSTALL_DIR/orqella"

if [ "$INSTALL_DESKTOP" = yes ]; then
  if [ "$os" = Darwin ]; then
    say "Installing Orqella Desktop to ~/Applications"
    mkdir -p "$HOME/Applications" "$TMP_DIR/desktop"
    tar -xzf "$TMP_DIR/$desktop_asset" -C "$TMP_DIR/desktop" || die "could not extract the desktop app"
    app_src=$(find "$TMP_DIR/desktop" -maxdepth 2 -type d -name '*.app' | sed -n '1p')
    [ -n "$app_src" ] || die "the desktop archive did not contain an .app bundle"
    rm -rf "$HOME/Applications/Orqella Desktop.app"
    ditto "$app_src" "$HOME/Applications/Orqella Desktop.app" || die "could not copy Orqella Desktop.app"
    xattr -dr com.apple.quarantine "$HOME/Applications/Orqella Desktop.app" 2>/dev/null || true
    desktop_display="$HOME/Applications/Orqella Desktop.app"
  else
    say "Installing Orqella Desktop"
    data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
    app_dir="$data_home/orqella-desktop"
    app_path="$app_dir/orqella-desktop.AppImage"
    mkdir -p "$app_dir"
    cp "$TMP_DIR/$desktop_asset" "$app_path.new"
    chmod 0755 "$app_path.new"
    mv -f "$app_path.new" "$app_path"
    ln -sf "$app_path" "$INSTALL_DIR/orqella-desktop"

    icon_name=applications-internet
    if fetch "$BASE_URL/assets/icon-512.png" "$app_dir/orqella-desktop.png" 2>/dev/null; then
      icon_name="$app_dir/orqella-desktop.png"
    fi
    applications="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
    mkdir -p "$applications"
    cat > "$applications/orqella-desktop.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Orqella Desktop
Comment=Local-first apps, modules, and machines
Exec="$app_path" %U
Icon=$icon_name
Terminal=false
Categories=Utility;
StartupWMClass=orqella-desktop
EOF
    chmod 0644 "$applications/orqella-desktop.desktop"
    if command -v update-desktop-database >/dev/null 2>&1; then
      update-desktop-database "$applications" >/dev/null 2>&1 || true
    fi
    desktop_display="$app_path"
  fi
fi

DAEMON_BIN="$INSTALL_DIR/orqella"
[ -x "$DAEMON_BIN" ] || die "the daemon binary is missing at $DAEMON_BIN"
installed_version=$("$DAEMON_BIN" --version 2>/dev/null | awk '{print $2; exit}' || true)

service_state=skipped
if [ "$INSTALL_SERVICE" = yes ]; then
  manual_daemon=no
  if fetch_local "http://127.0.0.1:9477/healthz" >/dev/null 2>&1; then
    if [ "$os" = Linux ]; then
      systemctl --user is-active --quiet orqella.service 2>/dev/null || manual_daemon=yes
    else
      launchctl print "gui/$(id -u)/com.orqella.daemon" >/dev/null 2>&1 || manual_daemon=yes
    fi
  fi

  if [ "$manual_daemon" = yes ]; then
    service_state="skipped (another daemon is running)"
    warn "a daemon is already responding on port 9477 outside the per-user service."
    warn "stop it, then run '$DAEMON_BIN service install' to finish the service setup."
  else
    say "Installing the per-user service"
    if [ "$os" = Darwin ]; then
      launchctl bootout "gui/$(id -u)/com.orqella.daemon" 2>/dev/null || true
    fi
    if "$DAEMON_BIN" service install >/dev/null 2>&1; then
      service_state=installed
      if [ "$os" = Linux ] && command -v systemctl >/dev/null 2>&1; then
        systemctl --user restart orqella.service >/dev/null 2>&1 || true
      elif [ "$os" = Darwin ]; then
        launchctl kickstart -k "gui/$(id -u)/com.orqella.daemon" >/dev/null 2>&1 || true
      fi
      say "Waiting for the daemon"
      attempt=0
      healthy=no
      while [ "$attempt" -lt 30 ]; do
        if fetch_local "http://127.0.0.1:9477/healthz" >/dev/null 2>&1; then
          healthy=yes
          break
        fi
        attempt=$((attempt + 1))
        sleep 0.5
      done
      if [ "$healthy" != yes ]; then
        warn "the daemon did not become healthy on port 9477 yet; check '$DAEMON_BIN doctor'"
      fi
    else
      service_state="not installed"
      warn "could not install the per-user service; start it with '$DAEMON_BIN serve'"
    fi
  fi
fi

if [ "$INSTALL_FIREWALL" = yes ]; then
  "$DAEMON_BIN" firewall install || warn "could not add the Link firewall rule; run '$DAEMON_BIN firewall install' later"
else
  printf 'Link firewall: skipped. Run %s firewall install to allow direct LAN connections.\n' "$DAEMON_BIN"
fi

printf '\n'
printf 'Orqella %s is installed.\n' "${installed_version:-$DAEMON_VERSION}"
printf '  dashboard  http://orqella.localhost:9477/\n'
printf '  daemon     %s\n' "$DAEMON_BIN"
if [ "$INSTALL_DESKTOP" = yes ]; then
  printf '  desktop    %s\n' "$desktop_display"
fi
if [ "$INSTALL_SERVICE" = yes ]; then
  printf '  service    %s\n' "$service_state"
fi
printf '\n'

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf 'Add %s to PATH to run orqella from a new shell:\n' "$INSTALL_DIR"
    printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
    printf 'Add that line to ~/.profile, ~/.bashrc, or ~/.zshrc to keep it.\n\n'
    ;;
esac

existing=$(command -v orqella 2>/dev/null || true)
if [ -n "$existing" ] && [ "$existing" != "$DAEMON_BIN" ]; then
  warn "another orqella is earlier in PATH: $existing"
  warn "remove it or put $INSTALL_DIR first so the new install is used."
fi

printf 'Useful commands:\n'
printf '  %s doctor\n' "$DAEMON_BIN"
printf '  %s open\n' "$DAEMON_BIN"
printf '  %s uninstall\n' "$DAEMON_BIN"
