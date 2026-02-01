#!/bin/bash
# Install script for cronr on macOS systems

set -e

# Check if .cronr already exists in the home directory
if [ -d "$HOME/.cronr" ]; then
	echo "Error: $HOME/.cronr directory already exists. If you want to reinstall, remove this directory first."
	exit 1
fi

# Create the .cronr directory for logs
mkdir -p "$HOME/.cronr"

# Create the LaunchAgent plist file
PLIST_FILE="$HOME/Library/LaunchAgents/com.rrainn.cronr.plist"
mkdir -p "$HOME/Library/LaunchAgents"

# Create the plist file
cat > "$PLIST_FILE" << EOL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.rrainn.cronr</string>
	<key>ProgramArguments</key>
	<array>
		<string>$(which cronr)</string>
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

echo "Cronr LaunchAgent installed successfully!"
echo "Cronr will now start automatically when you log in"
