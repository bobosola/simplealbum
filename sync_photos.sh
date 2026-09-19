#!/usr/bin/env bash
#
# sync_photos.sh - Send local folder(s) or individual image(s) to a remote server via rsync over SSH
#

set -euo pipefail

# ==============================================================================
# CONFIGURATION (Edit these values to match your remote server)
# ==============================================================================
REMOTE_USER="your username here"       # Remote SSH username
REMOTE_HOST="your.domain.com"          # Remote IP or hostname / domain
REMOTE_DEST="path/to/your/album/"      # Default base destination directory on remote
SSH_PORT="22"                          # SSH port (default is 22)
SSH_KEY=""                             # Optional: path to specific key (e.g. "$HOME/.ssh/id_ed25519")
                                       # Leave empty ("") to use default SSH key / ssh-agent

# Remote Permissions & Ownership
# Leave empty to keep defaults, or set explicitly (e.g. REMOTE_CHOWN="username:www-data")
REMOTE_CHOWN=""
# Set permissions on remote: directories 775 (with setgid) and files 664
CHMOD_OPTS="--chmod=D2775,F664"
# ==============================================================================

# 1. Collect sources (files and/or folders)
SOURCES=()

# Split a line of user input into SOURCES the way a shell would, but without
# executing anything.
#
# Dragging a file into a terminal inserts shell-quoted text (spaces become
# `\ `, and the path may also be wrapped in quotes), so the input has to be
# unescaped. The previous implementation did that with
# `eval "SOURCES=($USER_INPUT)"`, which *also* runs command substitutions:
# pasting a path containing `$(...)` executed it. This parser only removes
# quoting and never expands anything.
parse_sources() {
    SOURCES=()
    local input="$1"
    local i=0 n=${#1}
    local ch word="" quote="" in_word=0

    while [ "$i" -lt "$n" ]; do
        ch="${input:$i:1}"
        if [ -n "$quote" ]; then
            if [ "$ch" = "$quote" ]; then
                quote=""
            elif [ "$ch" = '\' ] && [ "$quote" = '"' ]; then
                i=$((i + 1))
                word="${word}${input:$i:1}"
            else
                word="${word}${ch}"
            fi
        else
            case "$ch" in
                '\') i=$((i + 1)); word="${word}${input:$i:1}"; in_word=1 ;;
                "'"|'"') quote="$ch"; in_word=1 ;;
                [[:space:]])
                    if [ "$in_word" -eq 1 ]; then
                        SOURCES+=("$word")
                        word=""
                        in_word=0
                    fi
                    ;;
                *) word="${word}${ch}"; in_word=1 ;;
            esac
        fi
        i=$((i + 1))
    done

    if [ "$in_word" -eq 1 ]; then
        SOURCES+=("$word")
    fi
}

if [ "$#" -gt 0 ]; then
    SOURCES=("$@")
else
    echo "=========================================================="
    echo "            Photo & File Sync to Remote Server            "
    echo "=========================================================="
    echo -n "Enter or drag-and-drop file(s) or folder(s): "
    read -r USER_INPUT

    if [ -z "$USER_INPUT" ]; then
        echo "Error: No files or folders provided." >&2
        exit 1
    fi

    # Parse drag-and-dropped paths (handles quotes and escaped spaces)
    parse_sources "$USER_INPUT"
fi

# 2. Validate all source items exist
CLEANED_SOURCES=()
for item in "${SOURCES[@]}"; do
    # Expand tilde (~) if present
    if [[ "$item" == ~* ]]; then
        item="${item/#\~/$HOME}"
    fi

    if [ ! -e "$item" ]; then
        echo "Error: File or folder does not exist: $item" >&2
        exit 1
    fi

    # If it is a directory, remove trailing slash so rsync transfers the folder itself
    if [ -d "$item" ]; then
        item="${item%/}"
    fi

    CLEANED_SOURCES+=("$item")
done

# 3. Destination folder prompt (allows specifying an existing subfolder or new one)
echo ""
echo "Base remote directory: $REMOTE_DEST"
echo -n "Remote subfolder (press [Enter] for base directory, or enter subfolder name): "
read -r SUBFOLDER

TARGET_DEST="$REMOTE_DEST"
if [ -n "$SUBFOLDER" ]; then
    # If user provided an absolute path (starts with /), use it directly
    if [[ "$SUBFOLDER" == /* ]]; then
        TARGET_DEST="$SUBFOLDER"
    else
        # Otherwise, append it as a subfolder to REMOTE_DEST
        TARGET_DEST="${REMOTE_DEST%/}/${SUBFOLDER}"
    fi
fi

# Ensure TARGET_DEST ends with a trailing slash so rsync treats it as a directory
TARGET_DEST="${TARGET_DEST%/}/"

# 4. Show summary before transfer
echo ""
echo "Items to sync (${#CLEANED_SOURCES[@]}):"
for src in "${CLEANED_SOURCES[@]}"; do
    echo "  - $src"
done
echo "Remote destination: ${REMOTE_USER}@${REMOTE_HOST}:${TARGET_DEST}"
echo ""

# 5. Build SSH command
SSH_CMD="ssh -p $SSH_PORT"
if [ -n "$SSH_KEY" ]; then
    SSH_CMD="$SSH_CMD -i $SSH_KEY"
fi

# 6. Run rsync
# -a: Archive mode (preserve timestamps, permissions, recurse subfolders)
# --no-o --no-g: Do NOT send local user or group; lets remote server assign ownership/inherit parent group
# -v: Verbose output
# -P: Show real-time progress bar + support resuming partial files
# --mkpath: Automatically creates the destination directory hierarchy if missing
RSYNC_EXTRA_ARGS=(--no-o --no-g)

if [ -n "$CHMOD_OPTS" ]; then
    RSYNC_EXTRA_ARGS+=("$CHMOD_OPTS")
fi

if [ -n "$REMOTE_CHOWN" ]; then
    RSYNC_EXTRA_ARGS+=("--chown=$REMOTE_CHOWN")
fi

echo "Starting transfer... (you may be prompted for your SSH passphrase/password)"
echo "-------------------------------------------------------------------------"

rsync -avP --mkpath "${RSYNC_EXTRA_ARGS[@]}" -e "$SSH_CMD" "${CLEANED_SOURCES[@]}" "${REMOTE_USER}@${REMOTE_HOST}:${TARGET_DEST}"

echo "-------------------------------------------------------------------------"
echo "Transfer complete!"
