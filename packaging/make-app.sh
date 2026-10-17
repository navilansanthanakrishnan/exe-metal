#!/bin/zsh
# Build "EXE Metal.app" — a real Mac application that opens .exe files.
#
# The bundle is an AppleScript droplet wrapping the Rust binary. That is not
# decoration: when Finder opens a document with an application it sends an
# `odoc` Apple Event, not argv, and a plain binary never sees the path. A
# droplet has an `on open` handler, so the file actually arrives. The droplet
# does nothing but forward it to the binary in Resources.
#
#   ./packaging/make-app.sh            build into ./dist
#   ./packaging/make-app.sh --install  and copy into ~/Applications
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
APP_NAME="EXE Metal"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"
BUNDLE_ID="dev.navilan.exe-metal"

install=0
[[ "${1:-}" == "--install" ]] && install=1

echo "building release binary…"
cargo build --release

rm -rf "$DIST"
mkdir -p "$DIST"

echo "compiling droplet…"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
cat > "$SCRATCH/droplet.applescript" <<'APPLESCRIPT'
-- EXE Metal. `on run` opens the library; `on open` installs or launches a
-- file the user double-clicked. Both hand off to the binary and return, so the
-- droplet is never the thing holding the window open.

on run
	startWith("")
end run

on open theFiles
	repeat with f in theFiles
		startWith(POSIX path of (f as alias))
	end repeat
end open

on startWith(thePath)
	set binPath to POSIX path of (path to resource "exe-metal")
	if thePath is "" then
		set cmd to quoted form of binPath & " home"
	else
		set cmd to quoted form of binPath & " gui-open " & quoted form of thePath
	end if
	do shell script cmd & " > /dev/null 2>&1 & disown"
end startWith
APPLESCRIPT

osacompile -o "$APP" "$SCRATCH/droplet.applescript"

echo "installing binary into the bundle…"
mkdir -p "$APP/Contents/Resources"
cp "$ROOT/target/release/exe-metal" "$APP/Contents/Resources/exe-metal"
chmod +x "$APP/Contents/Resources/exe-metal"

echo "declaring the .exe document type…"
PLIST="$APP/Contents/Info.plist"
PB=/usr/libexec/PlistBuddy

set_plist() { $PB -c "$1" "$PLIST" >/dev/null 2>&1 || true; }

set_plist "Delete :CFBundleDocumentTypes"
set_plist "Add :CFBundleName string $APP_NAME"
set_plist "Set :CFBundleName $APP_NAME"
set_plist "Add :CFBundleIdentifier string $BUNDLE_ID"
set_plist "Set :CFBundleIdentifier $BUNDLE_ID"
set_plist "Add :CFBundleShortVersionString string 0.1.0"
set_plist "Set :CFBundleShortVersionString 0.1.0"
set_plist "Add :NSHighResolutionCapable bool true"
set_plist "Set :NSHighResolutionCapable true"
# A droplet defaults to "background-ish"; this makes it a normal app with a
# Dock icon, which is what a library window needs.
set_plist "Add :LSUIElement bool false"
set_plist "Set :LSUIElement false"
set_plist "Add :LSMinimumSystemVersion string 14.0"

# Accept .exe by extension and by UTI. Both are declared because a file
# downloaded from a browser often arrives with no UTI assigned at all, and then
# only the extension matches.
set_plist "Add :CFBundleDocumentTypes array"
set_plist "Add :CFBundleDocumentTypes:0 dict"
set_plist "Add :CFBundleDocumentTypes:0:CFBundleTypeName string Windows executable"
set_plist "Add :CFBundleDocumentTypes:0:CFBundleTypeRole string Viewer"
set_plist "Add :CFBundleDocumentTypes:0:LSHandlerRank string Alternate"
set_plist "Add :CFBundleDocumentTypes:0:CFBundleTypeExtensions array"
set_plist "Add :CFBundleDocumentTypes:0:CFBundleTypeExtensions:0 string exe"
set_plist "Add :CFBundleDocumentTypes:0:LSItemContentTypes array"
set_plist "Add :CFBundleDocumentTypes:0:LSItemContentTypes:0 string com.microsoft.windows-executable"

plutil -lint "$PLIST" >/dev/null

# Ad-hoc signature: unsigned bundles that contain a copied binary are killed on
# launch by Gatekeeper on recent macOS. This is not notarisation, it is the
# minimum that makes it runnable locally.
echo "signing (ad-hoc)…"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || \
  echo "  warning: ad-hoc signing failed; the app may need 'xattr -cr' before it opens"

if (( install )); then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/$APP_NAME.app"
  cp -R "$APP" "$HOME/Applications/"
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -f "$HOME/Applications/$APP_NAME.app" >/dev/null 2>&1 || true
  echo "installed to ~/Applications/$APP_NAME.app"
else
  echo "built $APP"
fi

cat <<EOF

Done.

  Open the library      open -a "$APP_NAME"
  Open a Windows file   open -a "$APP_NAME" /path/to/setup.exe

To make .exe files open with this by default, select one in Finder,
Get Info, change "Open with" to $APP_NAME, then Change All.
EOF
