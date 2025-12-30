#!/bin/bash
# tgcryptfs home directory sync daemon
# Syncs home directory to tgcryptfs mount with real-time watching

set -euo pipefail

# Configuration
MOUNT_POINT="${TGCRYPTFS_MOUNT:-$HOME/mnt/tgcryptfs}"
BACKUP_DIR="${TGCRYPTFS_BACKUP_DIR:-$MOUNT_POINT/home}"
PASSWORD_FILE="${TGCRYPTFS_PASSWORD_FILE:-$HOME/.local/share/tgcryptfs/encryption.key}"
EXCLUDES_FILE="${TGCRYPTFS_EXCLUDES:-$HOME/.local/share/tgcryptfs/rsync-excludes.txt}"
SYNC_INTERVAL="${TGCRYPTFS_SYNC_INTERVAL:-3600}"  # Default: 1 hour for full sync
WATCH_DEBOUNCE="${TGCRYPTFS_WATCH_DEBOUNCE:-30}"  # Seconds to wait after file change
TGCRYPTFS_BIN="${TGCRYPTFS_BIN:-$(which tgcryptfs 2>/dev/null || echo "$HOME/.local/bin/tgcryptfs")}"

# User ignore file (like .gitignore)
USER_IGNORE_FILE="$HOME/.tgcryptfsignore"

# Logging functions
log_info() {
    local msg="$1"
    if [[ "$OSTYPE" == "darwin"* ]]; then
        logger -p user.info -t tgcryptfs-sync "$msg"
    elif command -v logger &>/dev/null && journalctl --version &>/dev/null 2>&1; then
        logger -p user.info -t tgcryptfs-sync "$msg"
    fi
    echo "[INFO] $msg"
}

log_error() {
    local msg="$1"
    if [[ "$OSTYPE" == "darwin"* ]]; then
        logger -p user.error -t tgcryptfs-sync "$msg"
    elif command -v logger &>/dev/null && journalctl --version &>/dev/null 2>&1; then
        logger -p user.error -t tgcryptfs-sync "$msg"
    fi
    echo "[ERROR] $msg" >&2
}

log_debug() {
    if [[ "${TGCRYPTFS_DEBUG:-0}" == "1" ]]; then
        echo "[DEBUG] $1"
    fi
}

# Check if mount is active
is_mounted() {
    if /sbin/mount | grep -q "tgcryptfs on $MOUNT_POINT"; then
        if timeout 5 ls "$MOUNT_POINT" >/dev/null 2>&1; then
            return 0
        fi
    fi
    return 1
}

# Mount tgcryptfs if not mounted
ensure_mounted() {
    log_info "Checking if tgcryptfs is mounted at $MOUNT_POINT..."

    if is_mounted; then
        log_info "tgcryptfs already mounted at $MOUNT_POINT"
        return 0
    fi

    log_info "Mount not detected, attempting to mount at $MOUNT_POINT"
    if [[ ! -f "$PASSWORD_FILE" ]]; then
        log_error "Password file not found: $PASSWORD_FILE"
        return 1
    fi

    mkdir -p "$MOUNT_POINT"
    "$TGCRYPTFS_BIN" mount "$MOUNT_POINT" --password-file "$PASSWORD_FILE" 2>&1 || true
    sleep 5

    if ! is_mounted; then
        log_error "Failed to mount tgcryptfs"
        return 1
    fi
    log_info "tgcryptfs mounted successfully"
}

# Build combined excludes file
build_excludes() {
    local combined_excludes="/tmp/tgcryptfs-excludes-$$.txt"

    # Start with system excludes
    if [[ -f "$EXCLUDES_FILE" ]]; then
        cat "$EXCLUDES_FILE" > "$combined_excludes"
    fi

    # Append user's .tgcryptfsignore if it exists
    if [[ -f "$USER_IGNORE_FILE" ]]; then
        echo "" >> "$combined_excludes"
        echo "# From ~/.tgcryptfsignore" >> "$combined_excludes"
        cat "$USER_IGNORE_FILE" >> "$combined_excludes"
        log_debug "Added user excludes from $USER_IGNORE_FILE"
    fi

    echo "$combined_excludes"
}

# Run rsync
run_sync() {
    local mode="${1:-full}"
    log_info "Starting $mode sync of $HOME to $BACKUP_DIR"

    local combined_excludes
    combined_excludes=$(build_excludes)

    mkdir -p "$BACKUP_DIR"

    local start_time=$(date +%s)
    local rsync_opts="-avh --delete"

    # For incremental syncs, only update changed files
    if [[ "$mode" == "incremental" ]]; then
        rsync_opts="-avh --update"
    fi

    if /usr/bin/rsync $rsync_opts \
        --exclude-from="$combined_excludes" \
        "$HOME/" "$BACKUP_DIR/"; then
        local end_time=$(date +%s)
        local duration=$((end_time - start_time))
        log_info "Sync completed successfully in ${duration}s"
        rm -f "$combined_excludes"
        return 0
    else
        local exit_code=$?
        log_error "Sync failed with exit code $exit_code"
        rm -f "$combined_excludes"
        return 1
    fi
}

# Watch mode using fswatch (macOS) or inotifywait (Linux)
watch_and_sync() {
    log_info "Starting watch mode with ${WATCH_DEBOUNCE}s debounce"

    local combined_excludes
    combined_excludes=$(build_excludes)

    # Build fswatch exclusion patterns
    local fswatch_excludes=""
    while IFS= read -r pattern; do
        # Skip comments and empty lines
        [[ "$pattern" =~ ^#.*$ || -z "$pattern" ]] && continue
        # Convert glob to regex for fswatch
        fswatch_excludes="$fswatch_excludes --exclude '$HOME/$pattern'"
    done < "$combined_excludes"

    rm -f "$combined_excludes"

    if [[ "$OSTYPE" == "darwin"* ]]; then
        if ! command -v fswatch &>/dev/null; then
            log_error "fswatch not installed. Install with: brew install fswatch"
            log_info "Falling back to interval-based sync"
            daemon_loop
            return
        fi

        log_info "Using fswatch for real-time file monitoring"

        # Use fswatch with latency (debounce)
        eval fswatch -o -l "$WATCH_DEBOUNCE" \
            --exclude '.git/' \
            --exclude 'mnt/tgcryptfs/' \
            --exclude '.cache/' \
            --exclude 'Library/Caches/' \
            --exclude '.Trash/' \
            "$HOME" | while read -r _; do
            if is_mounted; then
                run_sync incremental || true
            fi
        done
    else
        if ! command -v inotifywait &>/dev/null; then
            log_error "inotifywait not installed. Install with: apt install inotify-tools"
            log_info "Falling back to interval-based sync"
            daemon_loop
            return
        fi

        log_info "Using inotifywait for real-time file monitoring"

        inotifywait -m -r -e modify,create,delete,move \
            --exclude '.git|mnt/tgcryptfs|\.cache|\.Trash' \
            "$HOME" | while read -r _; do
            sleep "$WATCH_DEBOUNCE"  # Debounce
            if is_mounted; then
                run_sync incremental || true
            fi
        done
    fi
}

# Main daemon loop (fallback for when fswatch/inotify not available)
daemon_loop() {
    log_info "tgcryptfs-sync daemon starting (interval: ${SYNC_INTERVAL}s)"

    while true; do
        if ensure_mounted; then
            run_sync full || true
        fi

        log_info "Sleeping for ${SYNC_INTERVAL}s until next sync"
        sleep "$SYNC_INTERVAL"
    done
}

# Hybrid mode: watch + periodic full sync
hybrid_daemon() {
    log_info "Starting hybrid sync mode (watch + periodic full sync)"

    # Start watcher in background
    watch_and_sync &
    local watch_pid=$!

    # Periodic full sync in foreground
    while true; do
        sleep "$SYNC_INTERVAL"
        if is_mounted; then
            log_info "Running periodic full sync"
            run_sync full || true
        fi
    done

    # Cleanup
    kill $watch_pid 2>/dev/null || true
}

# Single sync run
single_sync() {
    if ensure_mounted; then
        run_sync full
    else
        exit 1
    fi
}

# Show status
show_status() {
    echo "tgcryptfs-sync Status"
    echo "====================="
    echo ""

    if is_mounted; then
        echo "Mount: ACTIVE at $MOUNT_POINT"
        echo "Backup size: $(du -sh "$BACKUP_DIR" 2>/dev/null | cut -f1 || echo "N/A")"
    else
        echo "Mount: NOT MOUNTED"
    fi

    echo ""
    echo "Configuration:"
    echo "  Excludes file: $EXCLUDES_FILE"
    echo "  User ignore:   $USER_IGNORE_FILE $([ -f "$USER_IGNORE_FILE" ] && echo "(exists)" || echo "(not found)")"
    echo "  Sync interval: ${SYNC_INTERVAL}s"
    echo "  Watch debounce: ${WATCH_DEBOUNCE}s"

    echo ""
    echo "Processes:"
    pgrep -fl "tgcryptfs" || echo "  No tgcryptfs processes running"
}

# Initialize user ignore file
init_ignore() {
    if [[ -f "$USER_IGNORE_FILE" ]]; then
        echo "~/.tgcryptfsignore already exists"
        return 0
    fi

    cat > "$USER_IGNORE_FILE" << 'EOF'
# tgcryptfs user ignore file
# Patterns work like .gitignore - one pattern per line
# Lines starting with # are comments

# Example: ignore all .log files
# *.log

# Example: ignore a specific directory
# Projects/large-video-project/

# Example: ignore node_modules everywhere
# **/node_modules/
EOF

    echo "Created ~/.tgcryptfsignore - edit to add your custom exclusions"
}

# Main
case "${1:-daemon}" in
    daemon)
        daemon_loop
        ;;
    watch)
        if ensure_mounted; then
            watch_and_sync
        else
            exit 1
        fi
        ;;
    hybrid)
        if ensure_mounted; then
            hybrid_daemon
        else
            exit 1
        fi
        ;;
    sync|once)
        single_sync
        ;;
    status)
        show_status
        ;;
    init-ignore)
        init_ignore
        ;;
    *)
        echo "Usage: $0 {daemon|watch|hybrid|sync|once|status|init-ignore}"
        echo ""
        echo "Modes:"
        echo "  daemon      - Periodic sync every SYNC_INTERVAL seconds (default: 3600)"
        echo "  watch       - Real-time sync using fswatch/inotify"
        echo "  hybrid      - Watch mode + periodic full sync"
        echo "  sync/once   - Single sync run"
        echo "  status      - Show sync status"
        echo "  init-ignore - Create ~/.tgcryptfsignore template"
        exit 1
        ;;
esac
