#!/usr/bin/env bash
#
# KanRite installer — install the `rite` command onto your PATH.
#
#   ./install.sh [--dir DIR] [--version vX.Y.Z] [--from-source]
#   curl -fsSL https://raw.githubusercontent.com/kanrite/rite/v5/install.sh | bash
#   ... | bash -s -- --uninstall [--all] [--yes]      (or run rite-uninstall)
#
# Default target directory: /usr/local/bin if writable, else ~/bin.
# Downloads the prebuilt binaries for your platform from the GitHub release.
# Run from a clone with cargo installed, it builds from source instead
# (CONTRIBUTING.md). On a system the release doesn't cover (another OS or
# processor, git too old, libraries too old) it says so plainly. Windows has
# its own installer, install.ps1 (ticket 282).
#
# Installs two commands: `rite`, the command line interface, and `riteui`, the
# desktop board. Both come from the release zip and from a source build; the
# board's source build needs Trunk and the wasm32-unknown-unknown target (and
# WebKitGTK on Linux), and is skipped with a note when they're missing.
#
# On Linux, installing riteui also installs its launcher entry and icon into
# ~/.local/share ($XDG_DATA_HOME), which is where the dock and top bar take
# the board's icon from (from the clone, or else fetched for the release).
#
# It also writes `rite-uninstall` beside them (ticket 242): it removes what
# was installed, and asks before removing your settings and data
# (`--all` removes them, `--yes` doesn't ask). Projects are never touched.
#
# Then, in the folder you keep projects in:  rite init <project>
#
set -euo pipefail

REPO="kanrite/rite"
DIR=""
VERSION="latest"
FROM_SOURCE=0
UNINSTALL=0
UNINSTALL_ARGS=()
# A release zip on disk instead of a download (tests/test.sh, ticket 312).
ZIP=""

while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --from-source) FROM_SOURCE=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    --zip) ZIP="$2"; shift 2 ;;
    --all|--yes) UNINSTALL_ARGS+=("$1"); shift ;;
    *) shift ;;
  esac
done

# Uninstalling without --dir: wherever rite is on the PATH.
# An install from before the rename (ticket 309) has amd and no rite.
if [ "${UNINSTALL}" = "1" ] && [ -z "${DIR}" ]; then
  for name in rite amd; do
    if command -v "${name}" >/dev/null 2>&1; then
      DIR="$(dirname "$(command -v "${name}")")"
      break
    fi
  done
fi

if [ -z "${DIR}" ]; then
  if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
    DIR="/usr/local/bin"
  else
    DIR="${HOME}/bin"
  fi
fi

# Where this script is, when it's run from a file. Piped from curl there's
# no file, and the current directory must never be mistaken for a clone.
SELF=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
  SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi
# A clone of KanRite, not just any directory with a Cargo.toml.
in_clone() { [ -n "${SELF}" ] && [ -f "${SELF}/crates/rite-cli/Cargo.toml" ]; }

die() { printf 'install: %s\n' "$*" >&2; exit 1; }

TMP=""
cleanup() { if [ -n "${TMP}" ]; then rm -rf "${TMP}"; fi; }
trap cleanup EXIT

# Puts SRC in place as DIR/NAME without writing into the installed file:
# Linux refuses to ("Text file busy") while that binary is running, which
# left an install half done (ticket 098). A copy beside it, then a rename,
# replaces the directory entry in one step; a running process keeps the old
# file until it exits, and the next launch gets the new one. Same directory,
# so the rename never crosses a filesystem.
replace_bin() {
  local src="$1" name="$2" dest="${DIR}/$2" new
  new="${DIR}/.${name}.new.$$"
  if [ -n "$(running "${dest}")" ]; then
    RESTART="${RESTART} ${name}"
  fi
  cp "${src}" "${new}" || { rm -f "${new}"; die "couldn't copy ${name} into ${DIR}"; }
  chmod +x "${new}"
  mv -f "${new}" "${dest}" || { rm -f "${new}"; die "couldn't replace ${dest}"; }
}

# The ids of processes running the binary at PATH, where that can be told
# (Linux's /proc). Empty elsewhere, and when nothing is.
running() {
  local exe pid target
  [ -d /proc/self ] && [ -e "$1" ] || return 0
  target="$(readlink -f "$1")"
  for exe in /proc/[0-9]*/exe; do
    [ "$(readlink "${exe}" 2>/dev/null)" = "${target}" ] || continue
    pid="${exe#/proc/}"
    printf '%s ' "${pid%/exe}"
  done
}

RESTART=""

# The 4.x names, kept through 5.x as symlinks to the new programs (ticket
# 309), which say once a day that they're now rite and riteui. A symlink,
# so no second copy; `ln -sf` replaces a 4.x binary of that name, running
# or not, in one step.
link_old_names() {
  ln -sf rite "${DIR}/amd"
  if [ -e "${DIR}/riteui" ]; then
    ln -sf riteui "${DIR}/amdui"
  fi
}

# The desktop shows an app's icon in its dock and top bar from the app's
# launcher entry, matched by app id, not from the window. riteui's app id is
# `riteui` (Tauri leaves GTK's as the program name unless enableGTKAppId is
# set, which would also make every riteui window share one process), so the
# entry is riteui.desktop. The icons come from the source tree: a release
# download run outside a clone has none to install, and says so.
install_launcher() {
  local src data apps icons size
  [ "$(uname -s)" = "Linux" ] && [ -x "${DIR}/riteui" ] || return 0
  data="${XDG_DATA_HOME:-${HOME}/.local/share}"
  apps="${data}/applications"
  icons="${data}/icons/hicolor"
  # A 4.x install's entry, amdui.desktop, would show the board twice. Gone
  # first, even if the new one can't be made (ticket 312).
  rm -f "${apps}/amdui.desktop" "${icons}/scalable/apps/amdui.svg" \
    "${icons}/32x32/apps/amdui.png" "${icons}/128x128/apps/amdui.png" "${icons}/256x256/apps/amdui.png"
  if in_clone; then
    src="${SELF}/crates/rite-ui"
  else
    # A release install: the icons from the same version of the repository.
    src="$(fetch_icons)" || {
      printf 'No launcher for riteui: its icons could not be downloaded.\n' >&2
      return 0
    }
  fi
  mkdir -p "${apps}" "${icons}/scalable/apps"
  cp "${src}/app-icon.svg" "${icons}/scalable/apps/riteui.svg"
  # <size directory>:<generated file>, all made from app-icon.svg.
  for size in 32x32:32x32 128x128:128x128 256x256:128x128@2x; do
    mkdir -p "${icons}/${size%%:*}/apps"
    cp "${src}/icons/${size#*:}.png" "${icons}/${size%%:*}/apps/riteui.png"
  done
  cat > "${apps}/riteui.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=KanRite
GenericName=Kanban board
Comment=The KanRite desktop board
Exec="${DIR}/riteui"
Icon=riteui
Terminal=false
Categories=Development;ProjectManagement;
StartupWMClass=riteui
EOF
  # Caches some desktops read; harmless where they're not used or not there.
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "${icons}" 2>/dev/null || true
  fi
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "${apps}" 2>/dev/null || true
  fi
  echo "Installed the riteui launcher -> ${apps}/riteui.desktop"
}

# The launcher's icons for the installed release, from the repository at its
# tag, into a temporary folder whose path is printed.
fetch_icons() {
  local tag dir file
  tag="v$("${DIR}/rite" --version 2>/dev/null | awk '{print $NF}')"
  [ "${tag}" != "v" ] || return 1
  dir="${TMP:-$(mktemp -d)}/icons-src"
  mkdir -p "${dir}/icons"
  for file in app-icon.svg icons/32x32.png icons/128x128.png icons/128x128@2x.png; do
    curl -fsSL "https://raw.githubusercontent.com/${REPO}/${tag}/crates/rite-ui/${file}" \
      -o "${dir}/${file}" 2>/dev/null || return 1
  done
  printf '%s' "${dir}"
}

# Says plainly, before anything is downloaded or changed, when this system
# isn't one KanRite runs on, and why.
check_system() {
  local os arch version major minor
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}" in
    Linux|Darwin) ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
      die "this installer is for Linux and macOS. On Windows, in PowerShell: irm https://raw.githubusercontent.com/kanrite/rite/v5/install.ps1 | iex (or install inside WSL: https://learn.microsoft.com/windows/wsl/install)" ;;
    *) die "KanRite runs on Linux and macOS; this system (${os}) isn't supported." ;;
  esac
  case "${arch}" in
    arm64|aarch64|x86_64|amd64) ;;
    *) die "KanRite's downloads are for x86_64 and arm64 (aarch64) processors; this one is ${arch}. Building from source may work: see https://github.com/${REPO}/blob/main/CONTRIBUTING.md" ;;
  esac
  command -v git >/dev/null 2>&1 || die "KanRite needs git 2.29 or later, and git isn't installed."
  version="$(git --version | awk '{print $3}')"
  major="${version%%.*}"
  minor="${version#*.}"; minor="${minor%%.*}"
  if [ "${major}" -lt 2 ] 2>/dev/null || { [ "${major}" -eq 2 ] && [ "${minor}" -lt 29 ]; } 2>/dev/null; then
    die "KanRite needs git 2.29 or later; this system has git ${version}. Update git, then run the install again."
  fi
}

# The uninstaller for an install into BIN, with the launcher under DATA:
# one script, written beside rite as rite-uninstall, and also what
# `install.sh --uninstall` runs.
uninstaller() {
  local bin="$1" data="$2" method="${3:-}"
  cat <<HEAD
#!/usr/bin/env bash
# KanRite's uninstaller, written by its installer (ticket 242).
# kanrite install: ${method:-unknown}
#
#   rite-uninstall [--all] [--yes]
#
# Removes rite, riteui, this script and riteui's launcher and icons. Then asks
# whether to remove your KanRite settings and data too (--all: yes without
# asking; --yes: never asks, so with no --all they stay). Your projects (the
# tickets, documents and ideas in your git repositories) are never touched.
set -euo pipefail
BIN=$(printf '%q' "${bin}")
DATA=$(printf '%q' "${data}")
HEAD
  cat <<'BODY'
ALL=0
ASK=1
for arg in "$@"; do
  case "${arg}" in
    --all) ALL=1 ;;
    --yes) ASK=0 ;;
    -h|--help) sed -n '4,10p' "$0"; exit 0 ;;
  esac
done

removed=""
gone() { rm -f "$1" && removed="${removed}  $1"$'\n'; }

# amd and amdui are symlinks to rite and riteui since 5.0 (ticket 309), so
# they may dangle by the time they're reached: -L as well as -e.
for name in rite riteui amd amdui; do
  [ -e "${BIN}/${name}" ] || [ -L "${BIN}/${name}" ] || continue
  # Running from this very file (Linux's /proc tells), not just any rite.
  if [ -d /proc/self ]; then
    target="$(readlink -f "${BIN}/${name}")"
    for exe in /proc/[0-9]*/exe; do
      if [ "$(readlink "${exe}" 2>/dev/null)" = "${target}" ]; then
        printf 'Note: %s is running; it keeps working until it exits.\n' "${name}" >&2
        break
      fi
    done
  fi
  gone "${BIN}/${name}"
done
# What `rite update` keeps for --rollback, under either name.
for name in rite riteui amd amdui; do
  rm -f "${BIN}/${name}.previous"
done
for app in riteui amdui; do for file in applications/${app}.desktop icons/hicolor/scalable/apps/${app}.svg \
  icons/hicolor/32x32/apps/${app}.png icons/hicolor/128x128/apps/${app}.png \
  icons/hicolor/256x256/apps/${app}.png; do
  if [ -e "${DATA}/${file}" ]; then gone "${DATA}/${file}"; fi
done; done
if [ -d "${DATA}/applications" ] && command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q "${DATA}/applications" 2>/dev/null || true
fi

if [ -n "${removed}" ]; then
  printf 'Removed:\n%s' "${removed}"
else
  printf "KanRite's programs were not in %s.\n" "${BIN}"
fi

# Your own settings and data: kept unless you say. Called kanrite since
# 5.0, agile-md before; an agile-md left by the move is a link to kanrite
# (ticket 310), which rm -rf removes as a link.
state="${XDG_STATE_HOME:-${HOME}/.local/state}/kanrite"
present=()
for base in "${XDG_CONFIG_HOME:-${HOME}/.config}" "${XDG_STATE_HOME:-${HOME}/.local/state}" \
  "${XDG_DATA_HOME:-${HOME}/.local/share}"; do
  for name in kanrite agile-md; do
    if [ -d "${base}/${name}" ] || [ -L "${base}/${name}" ]; then present+=("${base}/${name}"); fi
  done
done
if [ "${#present[@]}" -gt 0 ]; then
  if [ "${ALL}" = "0" ] && [ "${ASK}" = "1" ] && { : </dev/tty; } 2>/dev/null; then
    printf '\nYour KanRite settings and data are still here:\n'
    printf '  %s\n' "${present[@]}"
    if [ -n "$(ls -A "${state}/recovered" 2>/dev/null)" ]; then
      printf '  (%s/recovered holds tickets saved after a push conflict: look before removing.)\n' "${state}"
    fi
    printf 'Remove them too? [y/N] '
    read -r answer </dev/tty || answer=""
    case "${answer}" in y|Y|yes|YES) ALL=1 ;; esac
  fi
  if [ "${ALL}" = "1" ]; then
    for d in "${present[@]}"; do rm -rf "${d}"; printf 'Removed %s\n' "${d}"; done
  else
    printf 'Kept your settings and data (rite-uninstall --all removes them):\n'
    printf '  %s\n' "${present[@]}"
  fi
fi

# Its own name since 5.0, and 4.x's (ticket 312).
rm -f "${BIN}/rite-uninstall" "${BIN}/amd-uninstall"
printf 'KanRite is uninstalled. Your projects, in your git repositories, were not touched.\n'
BODY
}

# Rust target triple for this machine — matches the assets this repository's
# release workflow publishes (amd-<target>.zip).
target_triple() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${arch}" in
    arm64|aarch64) arch="aarch64" ;;
    x86_64|amd64) arch="x86_64" ;;
    *) die "unsupported processor '${arch}'" ;;
  esac
  case "${os}" in
    Linux) printf '%s-unknown-linux-gnu' "${arch}" ;;
    Darwin) printf '%s-apple-darwin' "${arch}" ;;
    *) die "unsupported system '${os}'" ;;
  esac
}

# Whether this machine can build the desktop board: its page is compiled to
# WebAssembly by Trunk before riteui itself, which embeds it.
can_build_board() {
  command -v trunk >/dev/null 2>&1 \
    && rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$'
}

build_from_source() {
  # How this install was made (ticket 191): `rite update` only replaces a
  # release install, never a source build.
  METHOD=source
  in_clone || die "building from source is done in a clone of https://github.com/${REPO} (see CONTRIBUTING.md)"
  command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust from https://rustup.rs"
  # The builds themselves are `cargo xtask build` (crates/xtask, ticket
  # 318), the same commands on every system, Windows included.
  printf 'Building rite from source...\n'
  ( cd "${SELF}" && cargo xtask build --release rite )
  mkdir -p "${DIR}"
  replace_bin "${SELF}/target/release/rite" rite
  # The desktop board is optional: without its build tools, or without
  # WebKitGTK on Linux, rite is still installed and this says what's missing.
  if ! can_build_board; then
    printf 'Skipping the desktop board (riteui): it needs Trunk and the wasm target —\n' >&2
    printf '  cargo install --locked trunk && rustup target add wasm32-unknown-unknown\n' >&2
    printf 'and on Linux WebKitGTK and ALSA (libwebkit2gtk-4.1-dev libasound2-dev). Then run install.sh again.\n' >&2
    return 0
  fi
  printf 'Building the desktop board (riteui)...\n'
  if ( cd "${SELF}" && cargo xtask build --release riteui ); then
    replace_bin "${SELF}/target/release/riteui" riteui
  else
    printf 'The desktop board did not build (on Linux it needs libwebkit2gtk-4.1-dev and libasound2-dev);\n' >&2
    printf 'rite is installed without it.\n' >&2
  fi
}

download_release() {
  METHOD=release
  command -v curl >/dev/null 2>&1 || die "curl not found"
  command -v unzip >/dev/null 2>&1 || die "unzip not found — install it, or use --from-source"

  local triple url
  triple="$(target_triple)"
  if [ "${VERSION}" = "latest" ]; then
    url="https://github.com/${REPO}/releases/latest/download/amd-${triple}.zip"
  else
    url="https://github.com/${REPO}/releases/download/${VERSION}/amd-${triple}.zip"
  fi

  TMP="$(mktemp -d)"
  if [ -n "${ZIP}" ]; then
    cp "${ZIP}" "${TMP}/rite.zip" || return 1
  else
    printf 'Downloading %s\n' "${url}"
    curl -fsSL "${url}" -o "${TMP}/rite.zip" || return 1
  fi
  unzip -q -o "${TMP}/rite.zip" -d "${TMP}" || return 1
  if [ ! -f "${TMP}/rite" ] && [ -f "${TMP}/amd" ]; then
    die "${VERSION} is from before amd was renamed rite (5.0); install it with the 4.x installer: curl -fsSL https://raw.githubusercontent.com/kanrite/rite/v4/install.sh | bash -s -- --version ${VERSION}"
  fi
  [ -f "${TMP}/rite" ] || return 1
  mkdir -p "${DIR}"
  replace_bin "${TMP}/rite" rite
  # Guarded rather than assumed: `--version` can pin a release made before the
  # zip carried both binaries, and `rite gui` is the way in to the board there.
  if [ -f "${TMP}/riteui" ]; then
    replace_bin "${TMP}/riteui" riteui
    check_board_libraries
  fi
}

# A downloaded riteui is linked against WebKitGTK on Linux (ticket 085). It
# installs either way; this says what to add when the libraries are missing,
# rather than leaving a board that won't start with no reason given.
check_board_libraries() {
  [ "$(uname -s)" = "Linux" ] || return 0
  command -v ldd >/dev/null 2>&1 || return 0
  local missing
  missing="$(ldd "${DIR}/riteui" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ')"
  [ -n "${missing}" ] || return 0
  printf 'Note: the desktop board (riteui) needs libraries this machine lacks:\n  %s\n' "${missing}" >&2
  printf 'Install WebKitGTK 4.1 and ALSA, e.g. Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-0 libasound2t64\n' >&2
  printf '  Fedora: sudo dnf install webkit2gtk4.1 alsa-lib    Arch: sudo pacman -S webkit2gtk-4.1 alsa-lib\n' >&2
  printf 'rite itself works without them.\n' >&2
}

if [ "${UNINSTALL}" = "1" ]; then
  TMP="$(mktemp -d)"
  uninstaller "${DIR}" "${XDG_DATA_HOME:-${HOME}/.local/share}" > "${TMP}/rite-uninstall"
  bash "${TMP}/rite-uninstall" "${UNINSTALL_ARGS[@]+"${UNINSTALL_ARGS[@]}"}"
  exit
fi

check_system
if [ "${FROM_SOURCE}" = "1" ]; then
  build_from_source
elif [ -n "${ZIP}" ]; then
  download_release || die "couldn't install from ${ZIP}"
elif in_clone && command -v cargo >/dev/null 2>&1; then
  build_from_source
elif ! download_release; then
  if in_clone; then
    printf 'No prebuilt binary available; falling back to a source build.\n' >&2
    build_from_source
  else
    die "couldn't download KanRite ${VERSION} for $(target_triple) from https://github.com/${REPO}/releases — check your connection, or that the version exists."
  fi
fi

chmod +x "${DIR}/rite"

# A binary this system can't run (most often a Linux whose C library is
# older than the one the release was built against) is said as such.
if ! version_out="$("${DIR}/rite" --version 2>&1)"; then
  printf 'install: rite was copied to %s but does not run on this system:\n  %s\n' "${DIR}/rite" "${version_out}" >&2
  if printf '%s' "${version_out}" | grep -q GLIBC; then
    printf 'This Linux is older than KanRite'"'"'s release builds support (its C library, glibc, is too old).\n' >&2
    printf 'Building from source may work: see https://github.com/%s/blob/main/CONTRIBUTING.md\n' "${REPO}" >&2
  fi
  exit 1
fi
echo "Installed rite -> ${DIR}/rite"
echo "${version_out}"
if [ -f "${DIR}/riteui" ]; then
  chmod +x "${DIR}/riteui"
  echo "Installed riteui -> ${DIR}/riteui   (the desktop board)"
  install_launcher
fi
uninstaller "${DIR}" "${XDG_DATA_HOME:-${HOME}/.local/share}" "${METHOD:-unknown}" > "${DIR}/rite-uninstall"
chmod +x "${DIR}/rite-uninstall"
# A 4.x install's uninstaller, amd-uninstall (ticket 312): replaced by
# rite-uninstall, which removes the old names as well.
rm -f "${DIR}/amd-uninstall"
link_old_names
echo "To uninstall later:  ${DIR}/rite-uninstall"
case ":${PATH}:" in
  *":${DIR}:"*) ;;
  *) echo "Note: ${DIR} is not on your PATH. Add this to your shell profile:"
     echo "      export PATH=\"${DIR}:\$PATH\"" ;;
esac
echo "Then start a project, from the folder you keep projects in:"
echo "  rite init my-project      (or rite join my-project for a team's)"
if [ -x "${DIR}/riteui" ]; then
  echo "For the desktop board:  riteui   (or rite gui)"
fi
for name in ${RESTART}; do
  echo "Note: ${name} was running while it was replaced; restart it to use the new build."
done
