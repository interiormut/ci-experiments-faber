#!/bin/sh
# faber agent installer. Served by faber itself, with the API URL below
# filled in at request time — this file is a template, not a script anyone
# should run from a checkout.
#
#   curl -fsSL https://faber.example.com/api/agent/install.sh | sh -s -- --token <token>
#
# Everything happens under $HOME and nothing here uses sudo. The daemon runs
# as a systemd *user* service with exactly the privileges of whoever runs
# this command, so an install that needed root would be granting the daemon
# more than the account that asked for it.

set -eu

API="@FABER_API@"
TOKEN=""
START="yes"

while [ $# -gt 0 ]; do
    case "$1" in
        --token) TOKEN="${2:-}"; shift 2 ;;
        # Writes the service unit but leaves it stopped, for anyone who
        # supervises this some other way.
        --no-start) START="no"; shift ;;
        *) echo "faber-agent install: unrecognized argument '$1'" >&2; exit 2 ;;
    esac
done

if [ -z "$TOKEN" ]; then
    echo "faber-agent install: --token is required." >&2
    echo "get one from the host's page in faber, then:" >&2
    echo "  curl -fsSL $API/api/agent/install.sh | sh -s -- --token <token>" >&2
    exit 2
fi

ARCH="$(uname -m)"
case "$ARCH" in
    # Only what faber actually builds. Naming the architecture beats letting
    # the download 404 into a confusing message, and beats far more the
    # alternative of handing over an x86_64 binary that dies later with
    # "exec format error".
    x86_64|amd64) ARCH="x86_64" ;;
    *)
        echo "faber-agent install: no agent binary is built for $ARCH (only x86_64 today)." >&2
        exit 1
        ;;
esac

OS="$(uname -s)"
if [ "$OS" != "Linux" ]; then
    echo "faber-agent install: the agent daemon is Linux-only; this is $OS." >&2
    exit 1
fi

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
BIN="$BIN_DIR/faber-agent"
mkdir -p "$BIN_DIR"

# Downloaded beside its destination and moved into place, so an interrupted
# transfer leaves no half-written binary a service unit might later exec.
TMP="$(mktemp "$BIN_DIR/.faber-agent.XXXXXX")"
trap 'rm -f "$TMP"' EXIT INT TERM

URL="$API/api/agent/binary/$ARCH"
echo "downloading $URL"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$TMP"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$TMP" "$URL"
else
    echo "faber-agent install: neither curl nor wget is available." >&2
    exit 1
fi

if [ ! -s "$TMP" ]; then
    echo "faber-agent install: the download was empty." >&2
    exit 1
fi

chmod 755 "$TMP"
mv -f "$TMP" "$BIN"
trap - EXIT INT TERM
echo "installed $BIN"

# The binary takes it from here: it generates a host keypair, trades the
# bootstrap token for a long-lived credential, and writes and starts its own
# systemd user unit. The token is single-use, so this runs exactly once.
if [ "$START" = "yes" ]; then
    "$BIN" install --token "$TOKEN" --api "$API"
else
    "$BIN" install --token "$TOKEN" --api "$API" --no-start
fi

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: $BIN_DIR is not on your PATH; the service uses the absolute path regardless." ;;
esac
