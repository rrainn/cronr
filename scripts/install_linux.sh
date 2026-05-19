#!/bin/bash
# Install script for cronr on Linux systems with systemd

set -e

# Check if running as root
if [ "$EUID" -ne 0 ]; then
	echo "Please run as root"
	exit 1
fi

# Detect direct root login vs. sudo invocation. When run directly as root
# (e.g. after `su -` or as the root user in a container) SUDO_USER is unset,
# so we cannot reliably locate a per-user cargo installation. Require the
# caller to use `sudo` from their normal account instead.
if [ -z "${SUDO_USER:-}" ] && [ "${USER:-root}" = "root" ]; then
	echo "Error: Please run this script with sudo from your normal user account:"
	echo "  sudo ./scripts/install_linux.sh"
	echo "Running as root directly (e.g. via su) means cargo cannot be located."
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

# Always build a fresh release binary so the installed version matches HEAD.
# Run cargo as the invoking user because root's PATH typically does not include
# the user-local cargo installation. We locate the binary directly rather than
# relying on the login shell sourcing the right PATH init files.
REAL_USER="${SUDO_USER:-$USER}"
REAL_HOME=$(getent passwd "$REAL_USER" | cut -d: -f6)

CARGO_BIN=""
for candidate in \
	"$REAL_HOME/.cargo/bin/cargo" \
	"/home/linuxbrew/.linuxbrew/bin/cargo" \
	"/usr/local/bin/cargo" \
	"/usr/bin/cargo" \
	"/opt/rust/bin/cargo" \
	"/opt/rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo" \
	"/opt/rustup/toolchains/stable-aarch64-unknown-linux-gnu/bin/cargo"; do
	if [ -x "$candidate" ]; then
		CARGO_BIN="$candidate"
		break
	fi
done

# Last-resort: search all home directories for a cargo binary
if [ -z "$CARGO_BIN" ]; then
	for candidate in /home/*/.cargo/bin/cargo; do
		if [ -x "$candidate" ]; then
			CARGO_BIN="$candidate"
			break
		fi
	done
fi

if [ -z "$CARGO_BIN" ]; then
	echo "Error: Cannot find cargo for user $REAL_USER."
	echo "Checked: ~/.cargo/bin, /home/linuxbrew/.linuxbrew/bin, /usr/local/bin, /usr/bin,"
	echo "         /opt/rust/bin, /opt/rustup/toolchains/*/bin, /home/*/.cargo/bin"
	echo "Install Rust via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
	echo "Then re-run: sudo ./scripts/install_linux.sh"
	exit 1
fi

# Ensure a C linker is available. Rust needs `cc` to link binaries. On a
# minimal system it may not be present, so we install the distro's build
# toolchain package automatically before attempting to compile.
if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
	echo "C linker not found. Installing build toolchain..."
	if command -v apt-get >/dev/null 2>&1; then
		apt-get install -y build-essential
	elif command -v dnf >/dev/null 2>&1; then
		dnf install -y gcc
	elif command -v yum >/dev/null 2>&1; then
		yum install -y gcc
	elif command -v apk >/dev/null 2>&1; then
		apk add --no-cache gcc musl-dev
	elif command -v pacman >/dev/null 2>&1; then
		pacman -S --noconfirm base-devel
	elif command -v zypper >/dev/null 2>&1; then
		zypper install -y gcc
	else
		echo "Error: No supported package manager found (apt, dnf, yum, apk, pacman, zypper)."
		echo "Please install a C compiler (e.g. gcc) manually, then re-run this script."
		exit 1
	fi
fi

echo "Building release binary as $REAL_USER (using $CARGO_BIN)..."
cd "$REPO_ROOT"
# Prepend cargo's own directory to PATH so cargo can locate rustc and other
# toolchain binaries that live alongside it (e.g. in a linuxbrew install).
CARGO_DIR="$(dirname "$CARGO_BIN")"
sudo -u "$REAL_USER" env PATH="$CARGO_DIR:$PATH" "$CARGO_BIN" build --release

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
