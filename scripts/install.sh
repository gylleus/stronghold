#!/usr/bin/env bash
# Builds Myrstack.app and installs it for the current user.
#
#   scripts/install.sh              release build, bundle, install ~/Applications/Myrstack.app and
#                                   point ~/.cargo/bin/myrstack at the bundled binary
#   scripts/install.sh --app-only   same, but leave ~/.cargo/bin/myrstack alone
#   scripts/install.sh --uninstall  remove the app (and the CLI link if it points into it)
#
# Nothing outside ~/Applications and ~/.cargo/bin is touched. MYRSTACK_SIGN_IDENTITY picks a
# codesign identity instead of ad hoc (see README).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
apps="$HOME/Applications"
app="$apps/Myrstack.app"
exe="$app/Contents/MacOS/myrstack"
cli="${CARGO_HOME:-$HOME/.cargo}/bin/myrstack"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

mode=install
case "${1:-}" in
  "") ;;
  --app-only) mode=app-only ;;
  --uninstall) mode=uninstall ;;
  *) echo "usage: $0 [--app-only | --uninstall]" >&2; exit 2 ;;
esac

if [[ $mode == uninstall ]]; then
  if [[ -L $cli && $(readlink "$cli") == "$exe" ]]; then
    rm "$cli"
    echo "removed $cli (reinstall the CLI with: cargo install --path $root)"
  fi
  if [[ -d $app ]]; then
    "$lsregister" -u "$app" 2>/dev/null || true
    rm -rf "$app"
    echo "removed $app"
  fi
  exit 0
fi

cargo build --release --manifest-path "$root/Cargo.toml"
python3 "$root/scripts/bundle.py" --binary "$root/target/release/myrstack" \
  --out "$root/target/release/Myrstack.app"

# Swap the new bundle in with a rename, so a half-copied app is never in place.
mkdir -p "$apps"
staging="$apps/.Myrstack.app.new"
rm -rf "$staging"
ditto "$root/target/release/Myrstack.app" "$staging"
rm -rf "$app"
mv "$staging" "$app"
# Register it so notification clicks and `open -b ai.depict.myrstack` find this copy.
"$lsregister" -f "$app" 2>/dev/null || true
echo "installed $app"

if [[ $mode == install ]]; then
  # A symlink keeps one binary: the CLI (`myrstack mcp`, `ctl`, `playbook`, …) and the app
  # are always the same build, and the bundled dylibs resolve through @executable_path, which
  # dyld takes from the link's target. Swapped in with a rename so running servers keep going.
  mkdir -p "$(dirname "$cli")"
  ln -sfn "$exe" "$cli.new"
  mv -f "$cli.new" "$cli"
  echo "linked $cli -> $exe"
fi
