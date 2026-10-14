#!/usr/bin/env bash
# Builds Stronghold.app and installs it for the current user.
#
#   scripts/install.sh              release build, bundle, install ~/Applications/Stronghold.app and
#                                   point ~/.cargo/bin/stronghold at the bundled binary
#   scripts/install.sh --app-only   same, but leave ~/.cargo/bin/stronghold alone
#   scripts/install.sh --uninstall  remove the app (and the CLI link if it points into it)
#
# Nothing outside ~/Applications and ~/.cargo/bin is touched. STRONGHOLD_SIGN_IDENTITY picks a
# codesign identity instead of ad hoc (see README).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
apps="$HOME/Applications"
app="$apps/Stronghold.app"
exe="$app/Contents/MacOS/stronghold"
cli="${CARGO_HOME:-$HOME/.cargo}/bin/stronghold"
# The app was called Myrstack until 2026-10-13. Its bundle is replaced, and its CLI link is kept
# pointing at the new binary so MCP registrations that run `myrstack mcp` keep working.
old_app="$apps/Myrstack.app"
old_exe="$old_app/Contents/MacOS/myrstack"
old_cli="${CARGO_HOME:-$HOME/.cargo}/bin/myrstack"
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
  if [[ -L $old_cli && $(readlink "$old_cli") == "$exe" ]]; then
    rm "$old_cli"
    echo "removed $old_cli"
  fi
  if [[ -d $app ]]; then
    "$lsregister" -u "$app" 2>/dev/null || true
    rm -rf "$app"
    echo "removed $app"
  fi
  exit 0
fi

cargo build --release --manifest-path "$root/Cargo.toml"

# A bundle left in a worktree's target/ is a second Stronghold.app with the same bundle id, and
# Launch Services may open that stale copy instead of the installed one; drop it.
built="$root/target/release/Stronghold.app"
if [[ -d $built ]]; then
  "$lsregister" -u "$built" 2>/dev/null || true
  rm -rf "$built"
fi

# Bundle straight into a staging copy and swap it in with a rename, so a half-copied app is
# never in place.
mkdir -p "$apps"
staging_dir="$apps/.Stronghold.staging"
staging="$staging_dir/Stronghold.app"
rm -rf "$staging_dir"
mkdir -p "$staging_dir"
python3 "$root/scripts/bundle.py" --binary "$root/target/release/stronghold" --out "$staging"
rm -rf "$app"
mv "$staging" "$app"
rmdir "$staging_dir"
# Register it so notification clicks and `open -b com.gylleus.stronghold` find this copy.
"$lsregister" -f "$app" 2>/dev/null || true
echo "installed $app"

# The old link moves first, so `myrstack mcp` never points at a removed bundle.
if [[ -L $old_cli && $(readlink "$old_cli") == "$old_exe" ]]; then
  ln -sfn "$exe" "$old_cli.new"
  mv -f "$old_cli.new" "$old_cli"
  echo "linked $old_cli -> $exe (old name, kept for existing MCP registrations)"
fi
if [[ -d $old_app ]]; then
  "$lsregister" -u "$old_app" 2>/dev/null || true
  rm -rf "$old_app"
  echo "removed $old_app (renamed to Stronghold)"
fi

if [[ $mode == install ]]; then
  # A symlink keeps one binary: the CLI (`stronghold mcp`, `ctl`, `playbook`, …) and the app
  # are always the same build, and the bundled dylibs resolve through @executable_path, which
  # dyld takes from the link's target. Swapped in with a rename so running servers keep going.
  mkdir -p "$(dirname "$cli")"
  ln -sfn "$exe" "$cli.new"
  mv -f "$cli.new" "$cli"
  echo "linked $cli -> $exe"
fi
