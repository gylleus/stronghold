# Quit and reopen Stronghold afterwards to run the new build.
# Release build → ~/Applications/Stronghold.app, with ~/.cargo/bin/stronghold linked to it.
install:
    scripts/install.sh

# Same as install, but leaves ~/.cargo/bin/stronghold alone.
install-app:
    scripts/install.sh --app-only

# Release build bundled into target/release/Stronghold.app, nothing installed.
bundle:
    cargo build --release
    scripts/bundle.py
