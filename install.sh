#!/bin/sh
# cctg installer (TASK-031): the client on Linux, macOS and Windows under Git
# Bash; with --hub, the hub on a server with Docker.
#
#   curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/<tag>/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --uninstall
#   curl -fsSL .../install.sh | sh -s -- --hub
#   sh install.sh --help
#
# The script of a tag installs the binary of the same tag (RELEASE below):
# script, configs and binary always belong together.
#
# The client writes only these files (<home> is $HOME, on Windows %USERPROFILE%):
#   <home>/.cctg/bin/cctg[.exe]         the binary; an update replaces it in place
#   <home>/.cctg/device.env             hub addresses, secret, certificate pin
#   <home>/.cctg/claude/mcp.json        the cctg channel server for claude
#   <home>/.cctg/claude/settings.json   cctg hooks and status line
#   <home>/.local/bin/claude-cctg       the wrapper (and claude-cctg.cmd on Windows)
# It never writes ~/.claude/settings.json or ~/.claude.json and never prints
# the hub secret (--hub prints it once, inside the client install line).
# Running it again updates; --uninstall removes these files.
#
# The body is one function called on the last line: a download cut short
# runs nothing.

main() {
set -eu

REPO=pockerhead/cctg
# The release this script belongs to. Bumped in the commit that gets the
# tag; release.yml refuses a tag that differs.
RELEASE=v0.1.1
# Marks every wrapper this script writes; --uninstall removes only those.
MARK=cctg-install
# device.env keys this script sets; other lines of the file are kept.
MANAGED='CCTG_HUB_SECRET|CCTG_HUB_AGENT_ADDR|CCTG_HUB_HOOK_ADDR|CCTG_HUB_CERT_SHA256'
# The other lines of device.env this script writes (--uninstall removes them).
ENV_HEADER='# cctg device config, written by install.sh (docs/remote-hub.md)'
HOST_MARK='# the CCTG_HOST line below: written by install.sh (macOS gives cctg no host name)'
# hub.env: the hub runs without a proxy (so the question is not asked again).
PROXY_NONE='# HTTPS_PROXY: none (install.sh --hub)'
AGENT_PORT=47291
HOOK_PORT=47292

hub_host=
hub_mode=0
hub_dir=
chat_id=
users=
proxy=
public_host=
agent_addr=
hook_addr=
pin=
secret_file=
from_source=0
yes=0
uninstall=0
while [ $# -gt 0 ]; do
    case $1 in
        --hub-host) need_value "$@"; hub_host=$2; shift 2 ;;
        --hub) hub_mode=1; shift ;;
        --dir) need_value "$@"; hub_dir=$2; shift 2 ;;
        --chat-id) need_value "$@"; chat_id=$2; shift 2 ;;
        --users) need_value "$@"; users=$2; shift 2 ;;
        --proxy) need_value "$@"; proxy=$2; shift 2 ;;
        --public-host) need_value "$@"; public_host=$2; shift 2 ;;
        --agent-addr) need_value "$@"; agent_addr=$2; shift 2 ;;
        --hook-addr) need_value "$@"; hook_addr=$2; shift 2 ;;
        --pin) need_value "$@"; pin=$2; shift 2 ;;
        --secret-file) need_value "$@"; secret_file=$2; shift 2 ;;
        --from-source) from_source=1; shift ;;
        -y|--yes) yes=1; shift ;;
        --uninstall) uninstall=1; shift ;;
        -h|--help) usage; return 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done

case $(uname -s) in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    MINGW*|MSYS*|CYGWIN*) os=windows ;;
    *) die "unsupported system $(uname -s)" ;;
esac
# Byte-wise patterns and ranges; Git Bash needs UTF-8 for its programs: in
# the C locale cygpath drops or garbles a non-ASCII user folder.
if [ "$os" = windows ]; then LC_ALL=C.UTF-8; else LC_ALL=C; fi
export LC_ALL
arch=$(uname -m)
case $arch in
    x86_64|amd64) arch=x86_64 ;;
    arm64|aarch64) arch=aarch64 ;;
esac
# A shell under Rosetta reports x86_64 on an Apple Silicon Mac.
if [ "$os" = macos ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
    arch=aarch64
fi

if [ "$os" = windows ]; then
    [ -n "${USERPROFILE:-}" ] || die "USERPROFILE is not set"
    home=$(cygpath -u "$USERPROFILE")
    ext=.exe
else
    [ -n "${HOME:-}" ] || die "HOME is not set"
    home=$HOME
    ext=
fi
root=$home/.cctg
bin_dir=$root/bin
exe=$bin_dir/cctg$ext
conf_dir=$root/claude
env_file=$root/device.env
wrap_dir=$home/.local/bin
wrapper=$wrap_dir/claude-cctg

tmp=$(mktemp -d)
stty_saved=
trap cleanup EXIT
trap 'exit 130' INT TERM

if [ "$hub_mode" = 1 ]; then
    if [ "$uninstall" = 1 ]; then uninstall_hub; else setup_hub; fi
    return 0
fi
if [ "$uninstall" = 1 ]; then
    uninstall_all
    return 0
fi

check_paths
read_settings
offer_claude
install_binary
write_device_env
write_claude_files
write_wrappers
say "installed $("$exe" --version)"
if [ -e "$bin_dir/cctg.hub-started" ]; then
    say "note: cctg supervise runs a hub from $bin_dir; it takes the new binary at its next restart"
fi
say "checking the hub (cctg doctor):"
"$exe" doctor </dev/null || say "the hub check failed; fix device.env and run $exe doctor again"
case ":$PATH:" in
    *":$wrap_dir:"*) ;;
    *) say "note: $wrap_dir is not in PATH; add it (Claude Code's installer uses the same folder)" ;;
esac
say "done: run claude-cctg in a project folder"
}

usage() {
    cat <<'EOF'
cctg installer. A device with Claude Code (Linux, macOS, Windows in Git
Bash): the cctg binary, device.env, Claude Code files and claude-cctg.

  --hub-host HOST       the hub's host name or IP (ports 47291 and 47292)
  --agent-addr H:P      the hub's agent address (instead of --hub-host)
  --hook-addr H:P       the hub's hook address (instead of --hub-host)
  --pin SHA256          the hub certificate's sha256 (needed for a hub on
                        another machine; the openssl fingerprint line works)
  --secret-file FILE    read the hub secret from the first line of FILE
                        (or set CCTG_HUB_SECRET; otherwise it is asked for)
  --from-source         build with cargo from the clone this script is in
  -y, --yes             ask nothing (also: install Claude Code when missing)
  --uninstall           remove what this script wrote
  CCTG_INSTALL_BASE_URL   where this release's files are (a mirror; http
                          only on this machine)

The hub on a server with Docker (docs/remote-hub.md):

  --hub                 set up or update the hub in --dir with docker compose
  --dir DIR             the hub's folder (default ~/cctg-hub)
  --chat-id -100N       the forum group
  --users ID[,ID]       Telegram user ids allowed to use the bot
  --proxy URL           HTTPS_PROXY for the Bot API (optional)
  --public-host HOST    how devices reach this server (for the client line;
                        needed without a terminal, kept for the next run)
  CCTG_BOT_TOKEN        the bot token (otherwise it is asked for)
  --hub --uninstall     stop the hub; hub.env, tls/ and the state volume stay

Running it again keeps the values already written unless new ones are
given; the script of a newer tag updates.
EOF
}

say() { printf '%s\n' "cctg-install: $*"; }
die() { printf '%s\n' "cctg-install: error: $*" >&2; exit 1; }
need_value() { [ $# -ge 2 ] || die "$1 needs a value"; }

cleanup() {
    if [ -n "$stty_saved" ]; then
        stty "$stty_saved" </dev/tty 2>/dev/null || true
    fi
    rm -rf "$tmp"
}

# The machine's own spelling of a path: C:/... on Windows.
native() {
    if [ "$os" = windows ]; then cygpath -m "$1"; else printf '%s' "$1"; fi
}

# The paths go into JSON, into shell commands of hooks and into the
# wrappers unescaped; refuse what would need escaping.
check_paths() {
    for p in "$(native "$root")" "$(native "$wrap_dir")"; do
        case $p in
            *[\"\$\`%\\]*) die "the path $p has a character the configs cannot carry" ;;
        esac
    done
}

interactive() {
    [ "$yes" = 0 ] && (: </dev/tty) 2>/dev/null
}

# ask <question> -> $answer (empty without a terminal)
ask() {
    answer=
    if interactive; then
        printf '%s' "$1" >/dev/tty
        IFS= read -r answer </dev/tty || answer=
    fi
}

# line_of <file> <key>: the last line of an env file that sets key (or empty).
line_of() {
    [ -f "$1" ] || return 0
    grep -E "^[[:space:]]*(export[[:space:]]+)?$2[[:space:]]*=" "$1" | tail -n 1 || true
}
old_line() { line_of "$env_file" "$1"; }

is_loopback() {
    case ${1%:*} in
        localhost|127.*|\[::1\]) return 0 ;;
        *) return 1 ;;
    esac
}

check_addr() {
    case $1 in
        *[!A-Za-z0-9.:_\[\]-]*|:*|*:) die "hub address $1 is not host:port" ;;
        *:*) ;;
        *) die "hub address $1 is not host:port" ;;
    esac
    port=${1##*:}
    case $port in
        ''|*[!0-9]*) die "hub address $1 is not host:port" ;;
    esac
}

# 64 hex digits; a whole openssl line (sha256 Fingerprint=AB:CD:..) works.
normalize_pin() {
    p=${1##*=}
    p=$(printf '%s' "$p" | tr -d ': ' | tr 'A-F' 'a-f')
    case $p in
        *[!0-9A-Fa-f]*) die "the pin is not a sha256 fingerprint (64 hex digits)" ;;
    esac
    [ ${#p} -eq 64 ] || die "the pin is not a sha256 fingerprint (64 hex digits)"
    pin=$p
}

check_secret() {
    case $secret in
        *[!!-~]*) die "the secret must be visible ASCII characters without spaces" ;;
    esac
    [ ${#secret} -ge 16 ] || die "the secret must have at least 16 characters"
}

# read_hidden <prompt> <what to set instead> -> $hidden, typed without echo.
read_hidden() {
    stty_saved=$(stty -g </dev/tty 2>/dev/null) || stty_saved=
    [ -n "$stty_saved" ] || die "cannot hide typing here; $2"
    printf '%s' "$1" >/dev/tty
    stty -echo </dev/tty
    IFS= read -r hidden </dev/tty || hidden=
    stty "$stty_saved" </dev/tty
    stty_saved=
    printf '\n' >/dev/tty
}

read_settings() {
    if [ -n "$hub_host" ]; then
        agent_addr=${agent_addr:-$hub_host:$AGENT_PORT}
        hook_addr=${hook_addr:-$hub_host:$HOOK_PORT}
    fi
    if [ -z "$agent_addr$hook_addr" ] && [ -z "$(old_line CCTG_HUB_AGENT_ADDR)$(old_line CCTG_HUB_HOOK_ADDR)" ]; then
        ask "Hub host (empty: the hub runs on this machine): "
        if [ -n "$answer" ]; then
            agent_addr=$answer:$AGENT_PORT
            hook_addr=$answer:$HOOK_PORT
        fi
    fi
    [ -z "$agent_addr" ] || check_addr "$agent_addr"
    [ -z "$hook_addr" ] || check_addr "$hook_addr"

    if [ -n "$pin" ]; then
        normalize_pin "$pin"
    elif [ -z "$(old_line CCTG_HUB_CERT_SHA256)" ]; then
        remote=0
        for a in "$agent_addr" "$hook_addr"; do
            if [ -n "$a" ] && ! is_loopback "$a"; then remote=1; fi
        done
        if [ "$remote" = 1 ]; then
            ask "Hub certificate sha256 (the hub logs it at start): "
            [ -n "$answer" ] || die "a hub on another machine needs --pin (see docs/remote-hub.md)"
            normalize_pin "$answer"
        fi
    fi

    secret=${CCTG_HUB_SECRET:-}
    # Nothing started from here gets it: cctg doctor must check the file,
    # as the sessions will.
    unset CCTG_HUB_SECRET
    if [ -n "$secret" ]; then
        :
    elif [ -n "$secret_file" ]; then
        [ "$os" != windows ] || secret_file=$(cygpath -u "$secret_file")
        [ -r "$secret_file" ] || die "cannot read $secret_file"
        IFS= read -r secret <"$secret_file" || true
        secret=$(printf '%s' "$secret" | tr -d '\r')
        [ -n "$secret" ] || die "$secret_file is empty (the secret is its first line)"
    elif [ -z "$(old_line CCTG_HUB_SECRET)" ]; then
        interactive || die "no hub secret: set CCTG_HUB_SECRET or use --secret-file"
        read_hidden "Hub secret (CCTG_HUB_SECRET of the hub, not shown): " \
            "set CCTG_HUB_SECRET or use --secret-file"
        secret=$hidden
        hidden=
        [ -n "$secret" ] || die "no hub secret typed"
    fi
    [ -z "$secret" ] || check_secret
}

have_claude() {
    command -v claude >/dev/null 2>&1 || [ -x "$wrap_dir/claude" ] || [ -x "$wrap_dir/claude.exe" ]
}

# Claude Code's own installer, as its setup page gives it
# (https://code.claude.com/docs/en/setup); never a copy of it.
offer_claude() {
    have_claude && return 0
    if [ "$yes" = 1 ]; then
        answer=y
    else
        ask "Claude Code is not installed. Install it with Anthropic's installer now? [Y/n] "
        interactive || answer=n
    fi
    case $answer in
        ''|y|Y|yes) ;;
        *) say "skipped Claude Code; install it later: https://code.claude.com/docs/en/setup"; return 0 ;;
    esac
    say "installing Claude Code with Anthropic's installer"
    if [ "$os" = windows ]; then
        powershell -NoProfile -Command "irm https://claude.ai/install.ps1 | iex" </dev/null || true
    elif command -v bash >/dev/null 2>&1; then
        curl -fsSL https://claude.ai/install.sh | bash || true
    else
        say "Anthropic's installer needs bash; install bash, then Claude Code"
    fi
    have_claude || say "Claude Code is still not found; see https://code.claude.com/docs/en/setup"
}

# The clone this script is in: its folder when run as a file, else (piped
# into sh) the current folder.
source_dir() {
    case $0 in
        *install.sh) cd "$(dirname "$0")" && pwd ;;
        *) pwd ;;
    esac
}

release_asset() {
    case $os-$arch in
        linux-x86_64) asset=cctg-$RELEASE-x86_64-unknown-linux-musl ;;
        macos-aarch64) asset=cctg-$RELEASE-aarch64-apple-darwin ;;
        windows-*) asset=cctg-$RELEASE-x86_64-pc-windows-msvc.exe ;;
        *) die "no release build for $os $arch; run this script from a clone with --from-source" ;;
    esac
}

# curl only: Claude Code's own installer needs it too. https only; plain
# http only to this machine (a test or a local mirror).
fetch() {
    case $1 in
        https://*) curl -fsSL --retry 3 --proto =https --proto-redir =https --tlsv1.2 -o "$2" "$1" ;;
        http://127.0.0.1[:/]*|http://localhost[:/]*) curl -fsSL --retry 3 --proto =http --proto-redir =http -o "$2" "$1" ;;
        *) die "$1: only https:// (http:// only on this machine)" ;;
    esac || die "download failed: $1"
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

install_binary() {
    command -v curl >/dev/null 2>&1 || [ "$from_source" = 1 ] || die "curl is needed"
    mkdir -p "$bin_dir"
    new=$bin_dir/cctg.install$ext
    if [ "$from_source" = 1 ]; then
        src=$(source_dir)
        [ -f "$src/Cargo.toml" ] || die "--from-source runs from a clone of $REPO"
        command -v cargo >/dev/null 2>&1 || die "--from-source needs cargo"
        say "building cctg with cargo"
        (cd "$src" && cargo build --release --locked -p cctg) || die "cargo build failed"
        cp "${CARGO_TARGET_DIR:-$src/target}/release/cctg$ext" "$new"
    else
        release_asset
        base=${CCTG_INSTALL_BASE_URL:-https://github.com/$REPO/releases/download/$RELEASE}
        base=${base%/}
        say "downloading $asset"
        fetch "$base/$asset" "$tmp/$asset"
        fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"
        want=$(awk -v n="$asset" '$2 == n || $2 == "*" n { print $1 }' "$tmp/SHA256SUMS")
        [ -n "$want" ] || die "SHA256SUMS has no line for $asset"
        [ "$(sha256 "$tmp/$asset")" = "$want" ] || die "checksum mismatch for $asset; nothing installed"
        mv -f "$tmp/$asset" "$new"
    fi
    if [ -f "$exe" ] && cmp -s "$new" "$exe"; then
        rm -f "$new"
        say "binary unchanged"
        return 0
    fi
    chmod 755 "$new"
    got=$("$new" --version 2>/dev/null) || { rm -f "$new"; die "the new binary does not run"; }
    case $got in
        "cctg "*) ;;
        *) rm -f "$new"; die "the new binary is not cctg" ;;
    esac
    if [ "$os" = windows ] && [ -e "$exe" ]; then
        # A running cctg.exe cannot be replaced, but it can be renamed:
        # sessions keep running from the old file (as with cctg deploy).
        old=$bin_dir/cctg.old.exe
        rm -f "$old" 2>/dev/null || true
        [ ! -e "$old" ] || old=$old.$(date +%s)
        mv -f "$exe" "$old" || die "cannot move $exe aside"
        if ! move_in; then
            # Never leave the configs pointing at no binary.
            mv -f "$old" "$exe" || die "could not put the new binary in place; the old one is $old: rename it to $exe"
            rm -f "$new" 2>/dev/null || true
            die "could not put the new binary in place (a virus scanner holding it?); the old one is back, run this again"
        fi
    else
        mv -f "$new" "$exe"
    fi
    rm -f "$bin_dir"/cctg.old.exe.* 2>/dev/null || true
}

# The new binary to its place. A virus scanner can hold a fresh file for a
# moment: a few tries, with the waits of cctg deploy.
move_in() {
    for wait in 0 0.1 0.3 1; do
        sleep "$wait"
        mv -f "$new" "$exe" 2>/dev/null && return 0
    done
    return 1
}

# Writes stdin to $1 (mode $2) only when the bytes differ: an unchanged
# settings file must keep its time, or every session would restart on
# the next "Обновить".
put() {
    cat >"$1.tmp"
    chmod "$2" "$1.tmp"
    if [ -f "$1" ] && cmp -s "$1.tmp" "$1"; then
        rm -f "$1.tmp"
    else
        mv -f "$1.tmp" "$1"
    fi
}

# A value in single quotes, ' as '\'': literal for sh and for dotenvy
# (no $ expansion inside single quotes).
squote() {
    q=
    rest=$1
    while :; do
        case $rest in
            *\'*) q="$q${rest%%\'*}'\\''"; rest=${rest#*\'} ;;
            *) break ;;
        esac
    done
    printf "'%s%s'" "$q" "$rest"
}

managed_line() {
    if [ -n "$2" ]; then
        printf '%s=%s\n' "$1" "$(squote "$2")"
    else
        old_line "$1"
    fi
}

write_device_env() {
    mkdir -p "$root"
    host_line=
    # Without /proc a Mac has no host name for cctg (the hook and the agent
    # read CCTG_HOST, else /proc, else HOSTNAME, which is not exported).
    if [ "$os" = macos ] && [ -z "$(old_line CCTG_HOST)" ]; then
        host_line="CCTG_HOST=$(scutil --get LocalHostName 2>/dev/null || hostname -s)"
    fi
    old_umask=$(umask)
    umask 077
    {
        if [ -f "$env_file" ]; then
            grep -v -E "^[[:space:]]*(export[[:space:]]+)?($MANAGED)[[:space:]]*=" "$env_file" || true
        else
            printf '%s\n' "$ENV_HEADER"
        fi
        [ -z "$host_line" ] || printf '%s\n' "$HOST_MARK" "$host_line"
        managed_line CCTG_HUB_SECRET "$secret"
        managed_line CCTG_HUB_AGENT_ADDR "$agent_addr"
        managed_line CCTG_HUB_HOOK_ADDR "$hook_addr"
        managed_line CCTG_HUB_CERT_SHA256 "$pin"
    } | put "$env_file" 600
    umask "$old_umask"
    secret=
}

write_claude_files() {
    mkdir -p "$conf_dir"
    c=$(native "$exe")
    put "$conf_dir/mcp.json" 644 <<EOF
{
  "mcpServers": {
    "cctg": { "command": "$c", "args": ["agent"] }
  }
}
EOF
    put "$conf_dir/settings.json" 644 <<EOF
{
  "statusLine": { "type": "command", "command": "\"$c\" statusline" },
  "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook SessionStart" }] }],
    "SessionEnd": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook SessionEnd" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook UserPromptSubmit" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook Stop" }] }],
    "SubagentStart": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook SubagentStart" }] }],
    "SubagentStop": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook SubagentStop" }] }],
    "PreToolUse": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook ToolStatus", "async": true }] }],
    "PostToolUse": [
      { "matcher": "SubagentHandback", "hooks": [{ "type": "command", "command": "\"$c\" hook PostToolUse" }] },
      { "hooks": [{ "type": "command", "command": "\"$c\" hook ToolStatus", "async": true }] }
    ],
    "PostToolUseFailure": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook ToolStatus", "async": true }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook PermissionRequest", "timeout": 100 }] }]
  }
}
EOF
}

# A file of ours at $1 carries $MARK; anything else there is moved aside once.
make_room() {
    if [ -e "$1" ] && ! grep -q "$MARK" "$1" 2>/dev/null; then
        aside=$1.before-cctg-install
        [ ! -e "$aside" ] || aside=$aside.$(date +%s)
        mv -f "$1" "$aside"
        say "moved the existing $1 to $aside"
    fi
}

write_wrappers() {
    mkdir -p "$wrap_dir"
    c=$(native "$exe")
    m=$(native "$conf_dir/mcp.json")
    s=$(native "$conf_dir/settings.json")
    # --settings (one value) goes last: the options before it take every
    # word up to the next option, so a prompt would become a channel name.
    make_room "$wrapper"
    put "$wrapper" 755 <<EOF
#!/bin/sh
# claude-cctg: Claude Code with the cctg channel and hooks ($MARK).
# Written by cctg install.sh; running it again rewrites this file.
exec "$c" run -- --mcp-config "$m" --dangerously-load-development-channels server:cctg --settings "$s" "\$@"
EOF
    if [ "$os" = windows ]; then
        cw=$(cygpath -w "$exe")
        mw=$(cygpath -w "$conf_dir/mcp.json")
        sw=$(cygpath -w "$conf_dir/settings.json")
        make_room "$wrapper.cmd"
        printf '%s\r\n' \
            '@echo off' \
            "rem claude-cctg: Claude Code with the cctg channel and hooks ($MARK)." \
            "\"$cw\" run -- --mcp-config \"$mw\" --dangerously-load-development-channels server:cctg --settings \"$sw\" %*" \
            | put "$wrapper.cmd" 644
    fi
}

remove() {
    if [ -e "$1" ]; then
        rm -f "$1" 2>/dev/null || true
        if [ -e "$1" ]; then say "left $1 (in use?)"; else say "removed $1"; fi
    fi
}

# device.env without the lines this script wrote; removed when nothing
# else is left in it.
strip_device_env() {
    [ -f "$env_file" ] || return 0
    rest=$(grep -v -E "^[[:space:]]*(export[[:space:]]+)?($MANAGED)[[:space:]]*=" "$env_file" \
        | grep -v -x -F "$ENV_HEADER" \
        | awk -v mark="$HOST_MARK" '
            host && index($0, "CCTG_HOST=") == 1 { host = 0; next }
            $0 == mark { host = 1; next }
            { host = 0; print }')
    if printf '%s\n' "$rest" | grep -q '[^[:space:]]'; then
        printf '%s\n' "$rest" | put "$env_file" 600
        say "kept the lines of $env_file this script did not write"
    else
        remove "$env_file"
    fi
}

uninstall_all() {
    for w in "$wrapper" "$wrapper.cmd"; do
        if [ -e "$w" ]; then
            if grep -q "$MARK" "$w" 2>/dev/null; then remove "$w"; else say "left $w (not written by this script)"; fi
        fi
        # What was there before the first install comes back.
        if [ ! -e "$w" ] && [ -e "$w.before-cctg-install" ]; then
            mv -f "$w.before-cctg-install" "$w"
            say "put back $w (it was $w.before-cctg-install)"
        fi
    done
    remove "$conf_dir/mcp.json"
    remove "$conf_dir/settings.json"
    strip_device_env
    remove "$exe"
    # cctg-workers: the agent's links to the binary (TASK-040).
    for f in "$bin_dir"/cctg.old.exe "$bin_dir"/cctg.old.exe.* "$bin_dir/cctg.install$ext" "$bin_dir"/cctg-workers/*; do
        remove "$f"
    done
    # ~/.local/bin stays: Claude Code's installer uses it too.
    for d in "$conf_dir" "$bin_dir/cctg-workers" "$bin_dir" "$root"; do
        rmdir "$d" 2>/dev/null || true
    done
    if [ -d "$root" ]; then
        say "left $root: files cctg made while running (spool, restart) or not written by this script"
    fi
    say "uninstalled; ~/.claude and ~/.claude.json were not touched"
}

# ------------------------------------------------------------- hub (--hub)

# A value of an env file line as it was written (surrounding quotes off).
value_of() {
    v=$(line_of "$1" "$2")
    v=${v#*=}
    case $v in
        \'*\') v=${v#\'}; v=${v%\'} ;;
        \"*\") v=${v#\"}; v=${v%\"} ;;
    esac
    printf '%s' "$v"
}

# hub_line <key> <new value>: KEY=value, or the line already in hub.env.
# compose reads hub.env; the values are checked to need no quoting.
hub_line() {
    if [ -n "$2" ]; then printf '%s=%s\n' "$1" "$2"; else line_of "$hub_env" "$1"; fi
}

# ask_kept <key> <question> <current value> -> $answer: the given value, or
# asked for when hub.env has none.
ask_kept() {
    answer=$3
    if [ -z "$answer" ] && [ -z "$(line_of "$hub_env" "$1")" ]; then
        ask "$2"
    fi
}

setup_hub() {
    command -v docker >/dev/null 2>&1 || die "--hub needs Docker with the compose plugin (docs/remote-hub.md)"
    docker compose version >/dev/null 2>&1 </dev/null || die "--hub needs the docker compose plugin"
    command -v openssl >/dev/null 2>&1 || die "--hub needs openssl for the hub certificate"
    hub_dir=${hub_dir:-$home/cctg-hub}
    [ "$os" != windows ] || hub_dir=$(cygpath -u "$hub_dir")
    hub_env=$hub_dir/hub.env
    # How devices reach this server, for the client line; kept in .env.
    public_host=${public_host:-$(value_of "$hub_dir/.env" CCTG_PUBLIC_HOST)}
    if [ -z "$public_host" ]; then
        interactive || die "no address for the devices: use --public-host HOST (this server's name or IP)"
        ask "How do devices reach this server (host name or IP): "
        public_host=$answer
    fi
    case $public_host in
        ''|*[!A-Za-z0-9.:_\[\]-]*) die "--public-host takes this server's host name or IP" ;;
    esac
    mkdir -p "$hub_dir/tls"

    # The compose files of the same tag as this script. One the user
    # changed (other ports, docs/remote-hub.md) stays and the new one goes
    # next to it; one as this script last wrote it (its hash is kept) is
    # updated.
    src=$(source_dir)
    raw=${CCTG_INSTALL_RAW_URL:-https://raw.githubusercontent.com/$REPO/$RELEASE}
    for f in compose.yml compose.host.yml; do
        if [ -f "$src/deploy/$f" ]; then
            cp "$src/deploy/$f" "$tmp/$f"
        else
            fetch "${raw%/}/deploy/$f" "$tmp/$f"
        fi
        if [ ! -f "$hub_dir/$f" ] || cmp -s "$tmp/$f" "$hub_dir/$f" \
            || [ "$(sha256 "$hub_dir/$f")" = "$(cat "$hub_dir/$f.installed-sha256" 2>/dev/null)" ]; then
            put "$hub_dir/$f" 644 <"$tmp/$f"
            sha256 "$hub_dir/$f" | put "$hub_dir/$f.installed-sha256" 644
        else
            put "$hub_dir/$f.new" 644 <"$tmp/$f"
            say "kept your changed $f; this release's one is $f.new (compare, then move it over)"
        fi
    done

    token=${CCTG_BOT_TOKEN:-}
    unset CCTG_BOT_TOKEN
    if [ -z "$token" ] && [ -z "$(line_of "$hub_env" CCTG_BOT_TOKEN)" ]; then
        interactive || die "no bot token: set CCTG_BOT_TOKEN"
        read_hidden "Bot token from @BotFather (not shown): " "set CCTG_BOT_TOKEN"
        token=$hidden
        hidden=
    fi
    case $token in
        *[!0-9A-Za-z:_-]*) die "the bot token has characters a token never has" ;;
        ''|[0-9]*:?*) ;;
        *) die "the bot token is not <digits>:<letters>" ;;
    esac
    ask_kept CCTG_CHAT_ID "Group chat id (-100...): " "$chat_id"
    chat_id=$answer
    case $chat_id in
        '') ;;
        -100*[!0-9]*|-100) die "the chat id is -100 and digits" ;;
        -100*) ;;
        *) die "the chat id is -100 and digits (Bot API form)" ;;
    esac
    ask_kept CCTG_ALLOWED_USER_IDS "Your Telegram user id (several: 1,2): " "$users"
    users=$answer
    case $users in
        *[!0-9,]*) die "user ids are digits separated by commas" ;;
    esac
    [ -n "$token$(line_of "$hub_env" CCTG_BOT_TOKEN)" ] || die "the bot token is needed"
    [ -n "$chat_id$(line_of "$hub_env" CCTG_CHAT_ID)" ] || die "--chat-id is needed"
    [ -n "$users$(line_of "$hub_env" CCTG_ALLOWED_USER_IDS)" ] || die "--users is needed"
    answer=$proxy
    if [ -z "$answer" ] && [ -z "$(line_of "$hub_env" HTTPS_PROXY)" ] \
        && ! grep -q -x -F "$PROXY_NONE" "$hub_env" 2>/dev/null; then
        ask "HTTPS proxy for Telegram (empty: none): "
    fi
    proxy=$answer
    case $proxy in
        *[\ \"\'\$\#\`\\]*) die "the proxy URL has a character hub.env cannot carry" ;;
    esac
    hub_secret=$(value_of "$hub_env" CCTG_HUB_SECRET)
    new_secret=
    if [ -z "$hub_secret" ]; then
        new_secret=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
        hub_secret=$new_secret
    fi

    old_umask=$(umask)
    umask 077
    {
        if [ -f "$hub_env" ]; then
            grep -v -E "^[[:space:]]*(export[[:space:]]+)?(CCTG_BOT_TOKEN|CCTG_CHAT_ID|CCTG_ALLOWED_USER_IDS|CCTG_HUB_SECRET|HTTPS_PROXY)[[:space:]]*=" "$hub_env" \
                | grep -v -x -F "$PROXY_NONE" || true
        else
            printf '%s\n' "# cctg hub settings, written by install.sh --hub (docs/remote-hub.md). Never commit."
        fi
        hub_line CCTG_BOT_TOKEN "$token"
        hub_line CCTG_CHAT_ID "$chat_id"
        hub_line CCTG_ALLOWED_USER_IDS "$users"
        hub_line CCTG_HUB_SECRET "$new_secret"
        hub_line HTTPS_PROXY "$proxy"
        [ -n "$proxy$(line_of "$hub_env" HTTPS_PROXY)" ] || printf '%s\n' "$PROXY_NONE"
    } | put "$hub_env" 600
    umask "$old_umask"
    token=

    # A proxy on the host's loopback needs the host network.
    compose_files=compose.yml
    case $(value_of "$hub_env" HTTPS_PROXY) in
        *://127.*|*://localhost*|*@127.*|*@localhost*) compose_files=compose.yml:compose.host.yml ;;
    esac
    # The image of this script's release: hub and clients are one version.
    printf '%s\n' "# written by install.sh --hub: how docker compose runs this hub" \
        "COMPOSE_FILE=$compose_files" \
        "CCTG_IMAGE_TAG=${RELEASE#v}" \
        "CCTG_PUBLIC_HOST=$public_host" | put "$hub_dir/.env" 644
    agent_port=$(device_port CCTG_AGENT_LISTEN "$AGENT_PORT")
    hook_port=$(device_port CCTG_HOOK_LISTEN "$HOOK_PORT")
    if [ -z "$agent_port" ] || [ -z "$hook_port" ]; then
        die "cannot read the hub's host ports from $hub_dir/compose.yml (\"HOST:CONTAINER\" lines under ports)"
    fi

    if [ ! -f "$hub_dir/tls/cert.pem" ] || [ ! -f "$hub_dir/tls/key.pem" ]; then
        say "making the hub certificate (self-signed, EC P-256, 10 years)"
        (
            cd "$hub_dir"
            # MSYS would turn /CN=... into a Windows path.
            MSYS_NO_PATHCONV=1 openssl ecparam -name prime256v1 -genkey -noout -out tls/key.pem
            MSYS_NO_PATHCONV=1 openssl req -x509 -new -key tls/key.pem -days 3650 \
                -subj /CN=cctg-hub -out tls/cert.pem
        ) >/dev/null 2>&1 </dev/null || die "openssl could not make the certificate"
    fi
    if [ "$(id -u)" = 0 ]; then
        chown 10001 "$hub_dir/tls/key.pem" && chmod 600 "$hub_dir/tls/key.pem"
    else
        chmod 644 "$hub_dir/tls/key.pem"
        say "note: tls/key.pem is readable by every user here (as root it would belong to uid 10001 only)"
    fi
    fp=$(cd "$hub_dir" && openssl x509 -in tls/cert.pem -noout -fingerprint -sha256 </dev/null) \
        || die "openssl cannot read tls/cert.pem"
    normalize_pin "$fp"

    say "starting the hub (docker compose in $hub_dir)"
    (cd "$hub_dir" && docker compose pull hub </dev/null)         || die "docker compose pull failed (the ghcr.io package must be public, or docker login ghcr.io)"
    started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    (cd "$hub_dir" && docker compose up -d --force-recreate hub </dev/null) || die "docker compose up failed"
    wait_hub

    if [ "$agent_port:$hook_port" = "$AGENT_PORT:$HOOK_PORT" ]; then
        where="--hub-host $public_host"
    else
        where="--agent-addr $public_host:$agent_port --hook-addr $public_host:$hook_port"
    fi
    say "the hub is up. On each device with Claude Code run (the line carries the hub secret: only your own machines, and clear it from chat history):"
    printf '\n%s\n\n' "curl -fsSL https://raw.githubusercontent.com/$REPO/$RELEASE/install.sh | CCTG_HUB_SECRET=$(squote "$hub_secret") sh -s -- $where --pin $pin"
    hub_secret=
}

# device_port <hub.env key> <default>: the port devices use for one of the
# hub's listeners. On the host network it is the listen port itself;
# otherwise the host port compose.yml publishes it on (empty: not found).
device_port() {
    p=$(value_of "$hub_env" "$1")
    p=${p##*:}
    case $p in
        ''|*[!0-9]*) p=$2 ;;
    esac
    case $compose_files in
        *compose.host.yml) printf '%s' "$p"; return 0 ;;
    esac
    sed -n "s/^[[:space:]]*-[[:space:]]*[\"']\{0,1\}\([^\"':]*:\)\{0,1\}\([0-9][0-9]*\):$p\(\/tcp\)\{0,1\}[\"']\{0,1\}[[:space:]]*\$/\2/p" \
        "$hub_dir/compose.yml" | head -n 1
}

# The hub's own start checks (bot token, group, topic rights) end in
# "hub started, polling"; a failed one ends the process with "Error: ...".
wait_hub() {
    i=0
    logs=
    while [ $i -lt 60 ]; do
        logs=$(cd "$hub_dir" && { docker compose logs --no-color --since "$started" hub 2>&1 </dev/null || true; })
        case $logs in
            *"hub started, polling"*) say "hub started: bot and group checked"; return 0 ;;
            *"Error: "*) break ;;
        esac
        sleep 2
        i=$((i + 1))
    done
    printf '%s\n' "$logs" | tail -n 20
    die "the hub did not start; its log is above (docker compose logs hub in $hub_dir)"
}

uninstall_hub() {
    hub_dir=${hub_dir:-$home/cctg-hub}
    [ "$os" != windows ] || hub_dir=$(cygpath -u "$hub_dir")
    [ -f "$hub_dir/compose.yml" ] || die "no hub set up by this script in $hub_dir"
    (cd "$hub_dir" && docker compose down </dev/null) || die "docker compose down failed"
    remove "$hub_dir/compose.yml"
    remove "$hub_dir/compose.host.yml"
    remove "$hub_dir/compose.yml.new"
    remove "$hub_dir/compose.host.yml.new"
    remove "$hub_dir/compose.yml.installed-sha256"
    remove "$hub_dir/compose.host.yml.installed-sha256"
    remove "$hub_dir/.env"
    say "stopped; $hub_dir keeps hub.env (token, secret) and tls/ (key), docker keeps the state volume: delete them by hand if you want"
}

main "$@"
