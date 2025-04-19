#!/bin/bash
# Install script for cronr on Linux systems with systemd

set -e

# Check if running as root
if [ "$EUID" -ne 0 ]; then
	echo "Please run as root"
	exit 1
fi

# Check if .cronr already exists in the home directory
if [ -d "$HOME/.cronr" ]; then
	echo "Error: $HOME/.cronr directory already exists. If you want to reinstall, remove this directory first."
	exit 1
fi

# Copy the service file to the systemd directory
cp "$(dirname "$0")/cronr.service" /etc/systemd/system/

# Reload systemd
systemctl daemon-reload

# Enable the service to start on boot
systemctl enable cronr.service

# Start the service
systemctl start cronr.service

echo "Cronr service installed and started successfully!"
echo "You can check the status with: systemctl status cronr.service"
