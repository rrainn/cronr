#!/bin/bash
# Install script for cronr on macOS systems.
# Supports both fresh installs and upgrades — safe to re-run.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# LaunchAgent plist path
PLIST_FILE="$HOME/Library/LaunchAgents/com.rrainn.cronr.plist"
mkdir -p "$HOME/Library/LaunchAgents"

# Detect whether this is an upgrade or a fresh install
IS_UPGRADE=false
if [ -f "$PLIST_FILE" ]; then
	IS_UPGRADE=true
fi

# Unload the existing LaunchAgent before replacing the binary so macOS
# releases its hold on the executable file.
launchctl bootout gui/$(id -u)/com.rrainn.cronr 2>/dev/null || true

# Build and install the binary via cargo so the installed version matches HEAD
echo "Building and installing cronr..."
cd "$REPO_ROOT"
cargo install --path .

if [ "$IS_UPGRADE" = false ]; then
	# Fresh install only: create the data directory for logs
	mkdir -p "$HOME/.cronr"
fi

# Write (or refresh) the plist so the binary path is always up to date.
# We regenerate it on every run in case cargo installed to a new location.
CRONR_BIN="$(which cronr)"
cat > "$PLIST_FILE" << EOL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.rrainn.cronr</string>
	<key>ProgramArguments</key>
	<array>
		<string>$CRONR_BIN</string>
		<string>start</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<false/>
	<key>StandardErrorPath</key>
	<string>$HOME/.cronr/daemon.log</string>
	<key>StandardOutPath</key>
	<string>$HOME/.cronr/daemon.log</string>
</dict>
</plist>
EOL

# Load the LaunchAgent
launchctl bootstrap gui/$(id -u) "$PLIST_FILE"

if [ "$IS_UPGRADE" = true ]; then
	echo "Cronr upgraded successfully!"
else
	echo "Cronr installed successfully!"
fi
echo "Cronr will start automatically when you log in."
echo "You can check the status with: launchctl list com.rrainn.cronr"
