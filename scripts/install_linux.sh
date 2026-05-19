#!/bin/bash
# Install script for cronr on Linux systems with systemd

set -e

# Check if running as root
if [ "$EUID" -ne 0 ]; then
	echo "Please run as root"
	exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY_SRC="$REPO_ROOT/target/release/cronr"
BINARY_DEST="/usr/local/bin/cronr"

# Detect whether this is an upgrade or a fresh install
UPGRADE=false
if [ -f "$BINARY_DEST" ]; then
	UPGRADE=true
fi

# On a fresh install, refuse to overwrite an existing data directory to avoid
# accidentally clobbering a previous deployment's jobs and logs.
if [ "$UPGRADE" = false ] && [ -d "/root/.cronr" ]; then
	echo "Error: /root/.cronr directory already exists. If you want to reinstall, remove this directory first."
	exit 1
fi

# Always build a fresh release binary so the installed version is up to date
echo "Building release binary..."
cd "$REPO_ROOT"
cargo build --release

# Stop the running service before replacing the binary to avoid "Text file busy"
if systemctl is-active --quiet cronr.service 2>/dev/null; then
	echo "Stopping cronr service..."
	systemctl stop cronr.service
fi

# Atomically replace the binary using a temp file + mv to avoid "Text file busy"
BINARY_TMP="$BINARY_DEST.tmp"
cp "$BINARY_SRC" "$BINARY_TMP"
chmod 755 "$BINARY_TMP"
mv -f "$BINARY_TMP" "$BINARY_DEST"
echo "Installed cronr to $BINARY_DEST"

if [ "$UPGRADE" = true ]; then
	# Restart the service to pick up the new binary
	systemctl start cronr.service
	echo "Cronr service restarted with updated binary."
else
	# Fresh install: set up the systemd service for the first time
	cp "$SCRIPT_DIR/cronr.service" /etc/systemd/system/
	systemctl daemon-reload
	systemctl enable cronr.service
	systemctl start cronr.service
	echo "Cronr service installed and started successfully!"
	echo "You can check the status with: systemctl status cronr.service"
fi
