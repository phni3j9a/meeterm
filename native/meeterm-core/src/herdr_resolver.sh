# Keep optional mise globs independent of the SSH login shell (e.g. zsh NOMATCH).
/bin/sh -c '
# Read-only, bounded candidate enumeration. Compatibility is checked in Rust.
# Do not source login files, activate a package manager, or start a runtime.
count=0
for candidate in \
    "$(command -v herdr 2>/dev/null || true)" \
    "$HOME/.local/bin/herdr" \
    "$HOME/.cargo/bin/herdr" \
    /usr/local/bin/herdr \
    /opt/homebrew/bin/herdr \
    "$HOME/.homebrew/bin/herdr" \
    "$HOME/.linuxbrew/bin/herdr" \
    /home/linuxbrew/.linuxbrew/bin/herdr \
    "$HOME/.local/share/mise/shims/herdr" \
    "$HOME/.local/share/mise/installs/herdr/current/bin/herdr" \
    "$HOME/.local/share/mise/installs/herdr/latest/bin/herdr" \
    "$HOME/.nix-profile/bin/herdr" \
    /nix/var/nix/profiles/default/bin/herdr \
    "$HOME/.local/share/mise/installs/herdr/"*/bin/herdr
do
    case "$candidate" in /*) ;; *) continue ;; esac
    [ -x "$candidate" ] && [ -f "$candidate" ] || continue
    printf "%s\000" "$candidate"
    count=$((count + 1))
    [ "$count" -lt 32 ] || break
done
[ "$count" -gt 0 ] || exit 127
'
