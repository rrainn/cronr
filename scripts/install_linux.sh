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

# Check if .cronr already exists in the home directory
if [ -d "/root/.cronr" ]; then
	echo "Error: /root/.cronr directory already exists. If you want to reinstall, remove this directory first."
	exit 1
fi

# Build the release binary if it doesn't exist
if [ ! -f "$BINARY_SRC" ]; then
	echo "Release binary not found. Building..."
	cd "$REPO_ROOT"
	cargo build --release
fi

# Copy the binary to the system path
cp "$BINARY_SRC" "$BINARY_DEST"
chmod 755 "$BINARY_DEST"
echo "Installed cronr to $BINARY_DEST"

# Copy the service file to the systemd directory
cp "$SCRIPT_DIR/cronr.service" /etc/systemd/system/

# Reload systemd
systemctl daemon-reload

# Enable the service to start on boot
systemctl enable cronr.service

# Start the service
systemctl start cronr.service

echo "Cronr service installed and started successfully!"
echo "You can check the status with: systemctl status cronr.service"
