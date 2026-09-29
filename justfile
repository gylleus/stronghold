# Quit and reopen Myrstack afterwards to run the new build.
# Release build → ~/Applications/Myrstack.app, with ~/.cargo/bin/myrstack linked to it.
install:
    scripts/install.sh

# Same as install, but leaves ~/.cargo/bin/myrstack alone.
install-app:
    scripts/install.sh --app-only

# Release build bundled into target/release/Myrstack.app, nothing installed.
bundle:
    cargo build --release
    scripts/bundle.py
