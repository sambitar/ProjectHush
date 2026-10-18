#!/usr/bin/env bash
# Register a downloaded Hush AppImage with the desktop, so the launcher and the
# taskbar show the brand mark instead of a placeholder.
#
# Why this exists: an AppImage installs nothing. The .desktop entry and the icons
# it needs live inside the file, and AppRun puts the AppDir on XDG_DATA_DIRS for
# the app's own process tree only. The shell that draws the launcher and the
# taskbar started at login with the session's XDG_DATA_DIRS, which does not
# include the AppDir, so it never sees Hush.desktop or hush-tauri.png.
#
# What the desktops then do differs, and that is what makes this look like a
# GNOME-vs-KDE bug rather than a packaging gap:
#
#   - GNOME Shell resolves a window through its WM class: the Wayland app_id
#     against StartupWMClass, then against .desktop ids, and gives up when both
#     miss — the window gets the generic placeholder. GTK takes that app_id from
#     argv[0], so it is "hush-tauri", which is exactly what the generated entry
#     carries in StartupWMClass. The entry is what is missing, not the name.
#   - Plasma keeps the window's own icon as a fallback (_NET_WM_ICON, which Tauri
#     does set on Linux), so on X11 it can show the logo with no desktop file at
#     all. On Wayland there is no window icon to fall back to and it cannot.
#
# Copying the entry and the icons into the user's own data directories fixes both:
# that is where the shell already looks, so no root and no package are needed, and
# the app keeps updating itself in place because Exec points at the AppImage file
# — the same file the updater rewrites.
#
# Usage:
#   scripts/install-appimage-desktop.sh                  # newest Hush*.AppImage found
#   scripts/install-appimage-desktop.sh ~/Downloads/Hush_0.5.0_amd64.AppImage
#   scripts/install-appimage-desktop.sh --dry-run
#   scripts/install-appimage-desktop.sh --uninstall
#
# Safe to re-run: the entry and the icons are written in place, so re-running
# after an AppImage has been updated is a no-op unless the path moved.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

APPIMAGE="${HUSH_APPIMAGE:-}"
DRY_RUN=0
UNINSTALL=0

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APPS_DIR="$DATA_HOME/applications"
ICONS_DIR="$DATA_HOME/icons"

# Where a downloaded AppImage usually ends up. Searched newest-first when no path
# is given, so the argument is optional in the common case.
SEARCH_DIRS=("$HOME/Applications" "$HOME/Downloads" "$HOME/Desktop" "$PWD")

say() { printf '%s\n' "$@" >&2; }
die() {
  printf '%s\n' "$@" >&2
  exit 1
}

# The leading comment block, so --help cannot drift out of step with the header.
usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h | --help) usage; exit 0 ;;
    --*)
      die "unknown option: $1" "" "Try --help."
      ;;
    *)
      [[ -z "$APPIMAGE" ]] || die "more than one AppImage given: $APPIMAGE and $1"
      APPIMAGE="$1"
      shift
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Finding the AppImage

newest_appimage() {
  local dir candidate best="" best_mtime=0 mtime
  for dir in "${SEARCH_DIRS[@]}"; do
    [[ -d "$dir" ]] || continue
    for candidate in "$dir"/Hush*.AppImage; do
      [[ -f "$candidate" ]] || continue
      mtime="$(stat -c %Y -- "$candidate" 2>/dev/null || echo 0)"
      if ((mtime > best_mtime)); then
        best="$candidate"
        best_mtime="$mtime"
      fi
    done
  done
  # Not `[[ -n $best ]] && printf`: this runs in a command substitution, and a
  # non-zero return there trips `set -e` in the caller before it can report why.
  if [[ -n "$best" ]]; then
    printf '%s\n' "$best"
  fi
}

resolve_appimage() {
  [[ -n "$APPIMAGE" ]] || APPIMAGE="$(newest_appimage)"
  [[ -n "$APPIMAGE" ]] || die \
    "No Hush AppImage given and none found in:" \
    "$(printf '  %s\n' "${SEARCH_DIRS[@]}")" \
    "" \
    "Pass the path: $0 ~/Downloads/Hush_0.5.0_amd64.AppImage"
  # `cd` + `pwd -P` so a relative path and a symlinked home both become the real
  # absolute path that goes into Exec=.
  [[ -e "$APPIMAGE" ]] || die "No such file: $APPIMAGE"
  APPIMAGE="$(cd "$(dirname "$APPIMAGE")" && pwd -P)/$(basename "$APPIMAGE")"
  [[ -f "$APPIMAGE" ]] || die "Not a regular file: $APPIMAGE"
}

# ---------------------------------------------------------------------------
# Reading the entry and the icons out of the bundle
#
# `--appimage-extract` is used rather than mounting through FUSE: it needs no
# fuse2fs and no /dev/fuse, which is the common case on a fresh machine and the
# only way this works inside a container.

extract_appdir() {
  local dest="$1"
  mkdir -p "$dest"
  if ! (cd "$dest" && "$APPIMAGE" --appimage-extract >/dev/null 2>&1); then
    die "$APPIMAGE could not be extracted." \
      "" \
      "It has to be executable first: chmod +x '$APPIMAGE'"
  fi
  [[ -d "$dest/squashfs-root" ]] || die "Extraction produced no squashfs-root for $APPIMAGE"
  printf '%s\n' "$dest/squashfs-root"
}

bundle_desktop_file() {
  local appdir="$1" found
  found="$(find "$appdir/usr/share/applications" -maxdepth 1 -name '*.desktop' -print -quit 2>/dev/null || true)"
  [[ -n "$found" ]] || die "No .desktop entry inside $APPIMAGE"
  printf '%s\n' "$found"
}

# The icon name the entry asks for. Kept as the bundle has it — installing the
# PNGs under that same name is what makes `Icon=` resolve, and inventing a name
# here would just be a second thing to keep in sync.
entry_icon_name() {
  local desktop="$1" name
  name="$(sed -n 's/^Icon=//p' "$desktop" | head -1)"
  [[ -n "$name" ]] || die "The bundled entry has no Icon= key"
  printf '%s\n' "$name"
}

# ---------------------------------------------------------------------------
# Writing the entry

# Exec= is parsed as a command line, so a path with a space has to be quoted, and
# a quote or backslash inside it has to be escaped.
quote_exec() {
  local path="$1"
  path="${path//\\/\\\\}"
  path="${path//\"/\\\"}"
  printf '"%s"' "$path"
}

# Every key from the bundle is passed through unchanged except the three we own,
# so the entry keeps one source of truth: whatever the bundler generates. Only
# Exec= has to change (the bundle names the binary inside the mount, which only
# exists while the AppImage is running) and only TryExec=/X-Hush-AppImage= are
# added. X-Hush-AppImage= is not decorative: it is what makes --uninstall exact
# and what lets a re-run recognise the entry it wrote.
write_entry() {
  local src="$1" icon_name="$2" out="$3" exec_line line
  exec_line="$(quote_exec "$APPIMAGE")"

  {
    printf '[Desktop Entry]\n'
    printf 'Exec=%s\n' "$exec_line"
    printf 'TryExec=%s\n' "$APPIMAGE"
    printf 'Icon=%s\n' "$icon_name"
    printf 'X-Hush-AppImage=%s\n' "$APPIMAGE"
    while IFS= read -r line; do
      case "$line" in
        '['*) continue ;;                                             # one group only; we emitted it
        Exec=* | TryExec=* | Icon=* | X-Hush-AppImage=*) continue ;;  # ours
        '') continue ;;
        *) printf '%s\n' "$line" ;;
      esac
    done <"$src"
  } >"$out"
}

# ---------------------------------------------------------------------------
# Installing the icons

install_icons() {
  local appdir="$1" src_root="$appdir/usr/share/icons/hicolor" size_dir size png dest count=0
  [[ -d "$src_root" ]] || die "No icons inside $APPIMAGE at usr/share/icons/hicolor"

  for size_dir in "$src_root"/*/apps; do
    [[ -d "$size_dir" ]] || continue
    size="$(basename "$(dirname "$size_dir")")"
    for png in "$size_dir"/*.png; do
      [[ -f "$png" ]] || continue
      dest="$ICONS_DIR/hicolor/$size/apps/$(basename "$png")"
      if ((DRY_RUN)); then
        say "  would install $dest"
      else
        install -D -m 0644 -- "$png" "$dest"
      fi
      count=$((count + 1))
    done
  done

  ((count > 0)) || die "No PNGs found inside $APPIMAGE under usr/share/icons/hicolor/*/apps"
  say "  installed $count icon file(s) under ${ICONS_DIR#"$HOME"/}/hicolor/"
}

refresh_caches() {
  if ((DRY_RUN)); then
    return 0
  fi

  # GTK finds an icon in a hicolor tree with no index.theme (verified), so the
  # cache is an optimisation here and not a requirement — and gtk-update-icon-cache
  # refuses to write one without index.theme anyway, so only run it when the
  # system has already provided that file in this directory.
  if [[ -f "$ICONS_DIR/hicolor/index.theme" ]] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$ICONS_DIR/hicolor" >/dev/null 2>&1 ||
      say "  note: gtk-update-icon-cache failed; GTK still reads the directory directly"
  fi
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPS_DIR" >/dev/null 2>&1 || true
  fi
}

# ---------------------------------------------------------------------------
# Verification
#
# The point of the whole script is that the shell can resolve Icon= to a file, so
# that is what is checked: a real GtkIconTheme lookup, not a path comparison. It
# runs in a fresh process, which is the closest thing to the shell's own lookup
# available from here.

verify_icon_lookup() {
  local name="$1" status=0
  if ! command -v python3 >/dev/null 2>&1 ||
    ! python3 -c "import gi; gi.require_version('Gtk','3.0')" 2>/dev/null; then
    say "  icon lookup: not checked (python3-gi absent)"
    return 0
  fi

  python3 - "$name" <<'PY' || status=$?
import sys
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk

name = sys.argv[1]
theme = Gtk.IconTheme.get_default()
theme.rescan_if_needed()
info = theme.lookup_icon(name, 128, 0)
if info is not None and info.get_filename():
    print("  icon lookup: FOUND", info.get_filename(), file=sys.stderr)
    sys.exit(0)
print("  icon lookup: NOT FOUND for", name, file=sys.stderr)
sys.exit(1)
PY

  if ((status != 0)); then
    say ""
    die "The shell cannot resolve Icon=$name to a file, so the launcher will still" \
      "show a placeholder. The files are under ${ICONS_DIR#"$HOME"/}/hicolor/ —" \
      "check that the name there matches the entry's Icon= key exactly."
  fi
  return 0
}

verify_entry() {
  local desktop="$1"
  if command -v desktop-file-validate >/dev/null 2>&1; then
    if desktop-file-validate "$desktop" 2>/dev/null; then
      say "  entry validates: $desktop"
    else
      say "  entry does NOT validate:"
      desktop-file-validate "$desktop" >&2 || true
    fi
  fi
}

# ---------------------------------------------------------------------------
# Uninstall

installed_entries() {
  local f
  for f in "$APPS_DIR"/*.desktop; do
    [[ -f "$f" ]] || continue
    if grep -q '^X-Hush-AppImage=' "$f" 2>/dev/null; then
      printf '%s\n' "$f"
    fi
  done
  return 0
}

do_uninstall() {
  local entries f icon removed=0
  entries="$(installed_entries)"

  if [[ -z "$entries" ]]; then
    say "Nothing to remove: no entry in $APPS_DIR carries X-Hush-AppImage."
    say "If Hush was installed some other way (a deb, or a hand-written entry),"
    say "this script did not write it and will not delete it."
    return 0
  fi

  while IFS= read -r f; do
    icon="$(sed -n 's/^Icon=//p' "$f" | head -1)"
    say "removing ${f#"$HOME"/}"
    rm -f -- "$f"
    removed=1
    if [[ -n "$icon" ]]; then
      for size_dir in "$ICONS_DIR"/hicolor/*/apps; do
        [[ -d "$size_dir" ]] || continue
        for png in "$size_dir/$icon".png "$size_dir/$icon".svg; do
          [[ -f "$png" ]] || continue
          say "removing ${png#"$HOME"/}"
          rm -f -- "$png"
        done
      done
    fi
  done <<<"$entries"

  if ((removed)) && command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPS_DIR" >/dev/null 2>&1 || true
  fi
  say "Done. The AppImage file itself is untouched."
}

# ---------------------------------------------------------------------------
# Main

if ((UNINSTALL)); then
  do_uninstall
  exit 0
fi

resolve_appimage
say "AppImage: $APPIMAGE"

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/hush-appimage-XXXXXX")"
cleanup() { rm -rf -- "$tmpdir"; }
trap cleanup EXIT

say "Extracting the bundle…"
appdir="$(extract_appdir "$tmpdir")"
src_desktop="$(bundle_desktop_file "$appdir")"
icon_name="$(entry_icon_name "$src_desktop")"
entry_name="$(basename "$src_desktop")"
dest_desktop="$APPS_DIR/$entry_name"

say "Entry: ${dest_desktop#"$HOME"/}"
say "Icon name: $icon_name"

if ((DRY_RUN)); then
  preview="$tmpdir/preview.desktop"
  write_entry "$src_desktop" "$icon_name" "$preview"
  say ""
  say "--- would write ${dest_desktop#"$HOME"/} ---"
  # Not /dev/stdout: writing a redirect target to it is not portable here, and a
  # preview file is easy to reason about.
  say "$(<"$preview")"
  say ""
  say "--- would install ---"
  install_icons "$appdir"
  say ""
  say "Dry run: nothing was written."
  exit 0
fi

[[ -x "$APPIMAGE" ]] || chmod +x -- "$APPIMAGE"

mkdir -p "$APPS_DIR"
write_entry "$src_desktop" "$icon_name" "$dest_desktop"
install_icons "$appdir"
refresh_caches

say ""
say "Verifying…"
verify_entry "$dest_desktop"
verify_icon_lookup "$icon_name"

say ""
say "Done. Hush should now appear in the launcher with the brand mark."
say ""
say "  Launcher entry : ${dest_desktop#"$HOME"/}"
say "  Icon           : ${ICONS_DIR#"$HOME"/}/hicolor/*/apps/$icon_name.png"
say "  StartupWMClass : $icon_name (must match the Wayland app_id GTK takes from argv[0])"
say ""
say "If the icon does not appear immediately, log out and back in: a shell that"
say "was already running when the icons landed may keep the old theme."
say "Undo with: $0 --uninstall"
