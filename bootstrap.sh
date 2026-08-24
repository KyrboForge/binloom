#!/bin/sh
# Download and run the latest Binloom release without installing it globally.
set -eu

RELEASE_URL="https://github.com/KyrboForge/binloom/releases/latest/download"

case "$(uname -s)" in
    Darwin) OS="macos" ;;
    Linux) OS="linux" ;;
    *)
        echo "error: unsupported operating system: $(uname -s)" >&2
        exit 1
        ;;
esac

case "$(uname -m)" in
    arm64 | aarch64) ARCH="aarch64" ;;
    x86_64 | amd64) ARCH="x86_64" ;;
    *)
        echo "error: unsupported architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

download_file() {
    if command -v curl >/dev/null 2>&1; then
        curl --fail --location --silent --show-error --output "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --output-document="$2" "$1"
    else
        echo "error: curl or wget is required to download Binloom" >&2
        return 1
    fi
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "error: sha256sum or shasum is required to verify Binloom" >&2
        return 1
    fi
}

ASSET="binloom_${OS}_${ARCH}.gz"
TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/binloom-bootstrap.XXXXXX")
ARCHIVE="$TEMP_DIR/$ASSET"
CHECKSUMS="$TEMP_DIR/SHA256SUMS"
BINARY="$TEMP_DIR/binloom"

cleanup() {
    rm -rf "$TEMP_DIR"
}

trap cleanup EXIT HUP INT TERM

download_file "$RELEASE_URL/$ASSET" "$ARCHIVE"
download_file "$RELEASE_URL/SHA256SUMS" "$CHECKSUMS"

EXPECTED_SHA256=$(awk -v asset="$ASSET" '
    {
        filename = $2
        sub(/^\*/, "", filename)

        if (filename == asset) {
            print $1
            exit
        }
    }
' "$CHECKSUMS")

if ! printf '%s\n' "$EXPECTED_SHA256" | grep -Eq '^[[:xdigit:]]{64}$'; then
    echo "error: checksum for $ASSET not found in SHA256SUMS" >&2
    exit 1
fi

ACTUAL_SHA256=$(sha256_file "$ARCHIVE")

if [ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]; then
    echo "error: Binloom checksum mismatch" >&2
    echo "expected: $EXPECTED_SHA256" >&2
    echo "actual:   $ACTUAL_SHA256" >&2
    exit 1
fi

gzip -dc "$ARCHIVE" > "$BINARY"
chmod 755 "$BINARY"

"$BINARY" "$@"
