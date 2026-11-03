#!/bin/sh
set -eu

state_dir=${1:-/var/lib/dux}
group=${2:-dux}
owner=${3:-root}

refuse() {
    printf 'dux: refusing unsafe index state path %s: %s\n' "$1" "$2" >&2
    exit 1
}

if [ -L "$state_dir" ] || [ ! -d "$state_dir" ]; then
    refuse "$state_dir" 'expected a real directory'
fi

# Remove every access bit before changing ownership. Mode 0700 is insufficient
# because an untrusted existing directory owner retains owner write access until
# chown completes. The package calls this under /var/lib/dux, whose parent is
# root-owned.
chmod 000 "$state_dir"
chown "$owner:$group" "$state_dir"
chmod 0700 "$state_dir"

for artifact in \
    "$state_dir/dux.db" \
    "$state_dir/dux.db-wal" \
    "$state_dir/dux.db-shm" \
    "$state_dir/dux.db-journal"; do
    if [ -L "$artifact" ]; then
        refuse "$artifact" 'symbolic links are not allowed'
    fi
    [ -e "$artifact" ] || continue
    [ -f "$artifact" ] || refuse "$artifact" 'expected a regular file'
    links=$(stat -c %h -- "$artifact") || refuse "$artifact" 'could not inspect link count'
    [ "$links" = 1 ] || refuse "$artifact" 'hard-linked files are not allowed'
    chown "$owner:$group" "$artifact"
    chmod 0640 "$artifact"
done

chmod 0750 "$state_dir"
