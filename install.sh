#!/bin/sh
# cctg installer (TASK-031): the client on Linux, macOS and Windows under Git
# Bash; with --hub, the hub on a server with Docker; with --hub --local
# (TASK-046), the hub on this machine without Docker, started at logon.
#
#   curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/<tag>/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --uninstall
#   curl -fsSL .../install.sh | sh -s -- --hub
#   curl -fsSL .../install.sh | sh -s -- --hub --local
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
#   one PATH line (marked "# cctg") in the shell's start file when
#   <home>/.local/bin is not in PATH: ~/.zshrc, ~/.bashrc or ~/.profile
# It never writes ~/.claude/settings.json or ~/.claude.json and never prints
# a secret: with --join the device trades a one-time code for its own secret
# (cctg join writes it into device.env); --hub prints such a code, inside
# the client install line.
# Running it again updates; --uninstall removes these files.
#
# The body is one function called on the last line: a download cut short
# runs nothing.

main() {
set -eu

REPO=pockerhead/cctg
# The release this script belongs to. Bumped in the commit that gets the
# tag; release.yml refuses a tag that differs.
RELEASE=v0.1.22
# Marks every wrapper this script writes; --uninstall removes only those.
MARK=cctg-install
# device.env keys this script sets; other lines of the file are kept.
MANAGED='CCTG_HUB_SECRET|CCTG_HUB_AGENT_ADDR|CCTG_HUB_HOOK_ADDR|CCTG_HUB_CERT_SHA256'
# The other lines of device.env this script writes (--uninstall removes them).
ENV_HEADER='# cctg device config, written by install.sh (docs/remote-hub.md)'
HOST_MARKED='# the CCTG_HOST line below: written by install.sh'
HOST_MARK="$HOST_MARKED (macOS gives cctg no host name)"
HOST_MARK_CONTAINER="$HOST_MARKED (in a container the host name is its id)"
HOST_MARK_GIVEN="$HOST_MARKED (--host)"
# The line that puts ~/.local/bin in PATH; --uninstall removes exactly it.
PATH_LINE="export PATH=\"\$HOME/.local/bin:\$PATH\" # cctg"
# hub.env: the hub runs without a proxy (so the question is not asked again).
PROXY_NONE='# HTTPS_PROXY: none (install.sh --hub)'
# hub.env of --hub --local: "$LISTEN_MARK <key>" marks a CCTG_*_LISTEN line
# this script chose; it is chosen again on every run. Unmarked ones are the
# user's and stay.
LISTEN_MARK='# written by install.sh --hub --local, rewritten on each run:'
AGENT_PORT=47291
HOOK_PORT=47292
# The local hub's autostart entry (--hub --local): the systemd user unit,
# the LaunchAgent label, the Windows Run value.
HUB_UNIT=cctg-hub.service
HUB_LABEL=io.github.pockerhead.cctg-hub
HUB_RUN_VALUE=cctg-hub

hub_host=
hub_mode=0
local_hub=0
hub_dir=
chat_id=
users=
proxy=
public_host=
agent_addr=
hook_addr=
pin=
secret_file=
join_code=${CCTG_JOIN_CODE:-}
host=
from_source=0
yes=0
uninstall=0
while [ $# -gt 0 ]; do
    case $1 in
        --hub-host) need_value "$@"; hub_host=$2; shift 2 ;;
        --hub) hub_mode=1; shift ;;
        --local) local_hub=1; shift ;;
        --dir) need_value "$@"; hub_dir=$2; shift 2 ;;
        --chat-id) need_value "$@"; chat_id=$2; shift 2 ;;
        --users) need_value "$@"; users=$2; shift 2 ;;
        --proxy) need_value "$@"; proxy=$2; shift 2 ;;
        --public-host) need_value "$@"; public_host=$2; shift 2 ;;
        --agent-addr) need_value "$@"; agent_addr=$2; shift 2 ;;
        --hook-addr) need_value "$@"; hook_addr=$2; shift 2 ;;
        --pin) need_value "$@"; pin=$2; shift 2 ;;
        --secret-file) need_value "$@"; secret_file=$2; shift 2 ;;
        --join) need_value "$@"; join_code=$2; shift 2 ;;
        --host) need_value "$@"; host=$2; shift 2 ;;
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

[ "$local_hub" = 0 ] || [ "$hub_mode" = 1 ] || die "--local goes with --hub"
if [ "$hub_mode" = 1 ]; then
    if [ "$local_hub" = 1 ]; then
        if [ "$uninstall" = 1 ]; then uninstall_local_hub; else setup_local_hub; fi
    elif [ "$uninstall" = 1 ]; then
        uninstall_hub
    else
        setup_hub
    fi
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
[ -z "$join_code" ] || join_hub
write_claude_files
write_wrappers
say "installed $("$exe" --version)"
if [ -e "$bin_dir/cctg.hub-started" ]; then
    say "note: cctg supervise runs a hub from $bin_dir; it takes the new binary at its next restart"
fi
say "checking the hub (cctg doctor):"
"$exe" doctor </dev/null || say "the hub check failed; fix device.env and run $exe doctor again"
offer_path
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
  --join CODE           a one-time join code of the hub (cctg hub code on
                        the hub, /devices in Telegram): this device gets
                        its own secret (or set CCTG_JOIN_CODE)
  --secret-file FILE    read the hub's shared secret from the first line of
                        FILE (or set CCTG_HUB_SECRET; without a join code
                        or a secret the code is asked for)
  --host NAME           this machine's name in the topic titles (CCTG_HOST);
                        in a container it is asked for, with --yes taken
                        from CCTG_HOST
  --from-source         build with cargo from the clone this script is in
  -y, --yes             ask nothing (also: install Claude Code when missing,
                        add ~/.local/bin to PATH in the shell's start file)
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

The hub on this machine, without Docker (the same questions; the proxy
stays only in hub.env, the hub reads it itself):

  --hub --local         set up or update the hub in --dir (default
                        ~/.cctg/hub): the binary, hub.env, cctg supervise
                        started at logon without a window (Windows: a Run
                        entry, Linux: a systemd user unit, macOS: a
                        LaunchAgent); log in <dir>/hub.log
  --public-host HOST    also serve other machines (TLS on all addresses);
                        without it only this machine
  --hub --local --uninstall
                        stop it and remove the autostart; hub.env, tls/,
                        state/ and the log stay

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
    unset CCTG_HUB_SECRET CCTG_JOIN_CODE
    if [ -n "$join_code" ]; then
        # The device's own secret comes from the hub (join_hub).
        secret=
        check_code
    elif [ -n "$secret" ]; then
        :
    elif [ -n "$secret_file" ]; then
        [ "$os" != windows ] || secret_file=$(cygpath -u "$secret_file")
        [ -r "$secret_file" ] || die "cannot read $secret_file"
        IFS= read -r secret <"$secret_file" || true
        secret=$(printf '%s' "$secret" | tr -d '\r')
        [ -n "$secret" ] || die "$secret_file is empty (the secret is its first line)"
    elif [ -z "$(old_line CCTG_HUB_SECRET)" ]; then
        interactive || die "no join code and no hub secret: use --join CODE (or CCTG_HUB_SECRET, --secret-file)"
        read_hidden "Join code from the hub, not shown (empty: type the hub's shared secret instead): " \
            "use --join CODE, set CCTG_HUB_SECRET or use --secret-file"
        join_code=$hidden
        hidden=
        if [ -n "$join_code" ]; then
            check_code
        else
            read_hidden "Hub secret (CCTG_HUB_SECRET of the hub, not shown): " \
                "use --join CODE, set CCTG_HUB_SECRET or use --secret-file"
            secret=$hidden
            hidden=
            [ -n "$secret" ] || die "no hub secret typed"
        fi
    fi
    [ -z "$secret" ] || check_secret
    choose_host
}

# XXXX-XXXX-XXXX-XXXX as the hub prints it; the hub checks the rest.
check_code() {
    case $join_code in
        *[!A-Za-z0-9\ -]*) die "a join code has letters, digits and dashes only" ;;
    esac
}

# The device's own secret for the join code: cctg join asks the hub (the
# address and pin just written) and puts the secret into device.env; it
# never prints it. A refused code leaves device.env as it was.
join_hub() {
    say "joining the hub with the code"
    CCTG_JOIN_CODE=$join_code "$exe" join </dev/null \
        || die "the hub did not enroll this device (above); run this again with a new join code"
    join_code=
}

check_host() {
    case $1 in
        ''|*[!A-Za-z0-9._-]*) die "the host name takes letters, digits, '.', '_' and '-' only" ;;
    esac
}

# A Docker, Podman or other container (a fake one in the tests: env
# container=...). A Mac is never one: containers there run Linux, and
# macOS gets its own stable name in write_device_env.
in_container() {
    [ "$os" != macos ] || return 1
    [ -f /.dockerenv ] || [ -f /run/.containerenv ] || env | grep -q '^container=' \
        || grep -q -E 'docker|containerd|kubepods|libpod|lxc' /proc/1/cgroup 2>/dev/null
}

# The CCTG_HOST line to write -> $host_line (empty: none) and $host_mark.
# --host always; in a container, where the host name is the container's id
# (a new device, and new topics, with every new container), unless
# device.env has one: asked, with --yes or without a terminal CCTG_HOST
# of the environment, else a warning.
choose_host() {
    host_line=
    host_mark=
    if [ -n "$host" ]; then
        check_host "$host"
        host_line=CCTG_HOST=$host
        host_mark=$HOST_MARK_GIVEN
        return 0
    fi
    if [ -n "$(old_line CCTG_HOST)" ] || ! in_container; then
        return 0
    fi
    name=${CCTG_HOST:-}
    if interactive; then
        if [ -z "$name" ]; then
            name=$(cat /proc/sys/kernel/hostname 2>/dev/null || hostname 2>/dev/null || true)
            # A container id (hex) is no name to suggest.
            case $name in
                *[!0-9a-f]*) ;;
                *) [ ${#name} -lt 12 ] || name= ;;
            esac
            case $name in
                ''|*[!A-Za-z0-9._-]*) name=container ;;
            esac
        fi
        ask "This is a container: its host name is its id. Name for the topics [$name]: "
        name=${answer:-$name}
    fi
    if [ -z "$name" ]; then
        say "warning: in a container the host name is the container's id, a new one for every container; give a name with --host NAME (or CCTG_HOST=NAME in $env_file)"
        return 0
    fi
    check_host "$name"
    host_line=CCTG_HOST=$name
    host_mark=$HOST_MARK_CONTAINER
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
    # Without /proc a Mac has no host name for cctg (the hook and the agent
    # read CCTG_HOST, else /proc, else HOSTNAME, which is not exported).
    if [ -z "$host_line" ] && [ "$os" = macos ] && [ -z "$(old_line CCTG_HOST)" ]; then
        host_line="CCTG_HOST=$(scutil --get LocalHostName 2>/dev/null || hostname -s)"
        host_mark=$HOST_MARK
    fi
    # --host replaces every CCTG_HOST line there is, and its mark.
    drop=$MANAGED
    [ -z "$host" ] || drop="$MANAGED|CCTG_HOST"
    old_umask=$(umask)
    umask 077
    {
        if [ -f "$env_file" ]; then
            grep -v -E "^[[:space:]]*(export[[:space:]]+)?($drop)[[:space:]]*=" "$env_file" \
                | if [ -n "$host" ]; then grep -v -F "$HOST_MARKED" || true; else cat; fi
        else
            printf '%s\n' "$ENV_HEADER"
        fi
        [ -z "$host_line" ] || printf '%s\n' "$host_mark" "$host_line"
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
    "PreToolUse": [
      { "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "\"$c\" hook PreToolUse", "timeout": 330, "statusMessage": "Вопрос в Telegram: ответьте там или кнопкой «В терминале»" }] },
      { "hooks": [{ "type": "command", "command": "\"$c\" hook ToolStatus", "async": true }] }
    ],
    "PostToolUse": [
      { "matcher": "SubagentHandback", "hooks": [{ "type": "command", "command": "\"$c\" hook PostToolUse" }] },
      { "hooks": [{ "type": "command", "command": "\"$c\" hook ToolStatus", "async": true }] }
    ],
    "PostToolUseFailure": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook ToolStatus", "async": true }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook PermissionRequest", "timeout": 100 }] }],
    "PreCompact": [{ "hooks": [{ "type": "command", "command": "\"$c\" hook PreCompact", "timeout": 5 }] }]
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

# The shell's start file for the PATH line: by $SHELL, Git Bash's ~/.bashrc.
rc_file() {
    if [ "$os" = windows ]; then
        printf '%s' "$home/.bashrc"
        return 0
    fi
    shell=${SHELL:-}
    case ${shell##*/} in
        zsh) printf '%s' "$home/.zshrc" ;;
        bash) printf '%s' "$home/.bashrc" ;;
        *) printf '%s' "$home/.profile" ;;
    esac
}

# ~/.local/bin not in PATH: the PATH line goes into the shell's start file
# once (asked; --yes adds it without asking).
offer_path() {
    case ":$PATH:" in
        *":$wrap_dir:"*) return 0 ;;
    esac
    rc=$(rc_file)
    if [ -f "$rc" ] && grep -q -x -F "$PATH_LINE" "$rc"; then
        say "note: $rc puts $wrap_dir in PATH; open a new terminal (or run: . $rc)"
        return 0
    fi
    if [ "$yes" = 1 ]; then
        answer=y
    else
        ask "$wrap_dir is not in PATH. Add it in $rc? [Y/n] "
        interactive || answer=n
    fi
    case $answer in
        ''|y|Y|yes) ;;
        *) say "note: $wrap_dir is not in PATH; add it (Claude Code's installer uses the same folder)"; return 0 ;;
    esac
    # A last line without its newline keeps its text.
    if [ -s "$rc" ] && [ -n "$(tail -c 1 "$rc")" ]; then
        printf '\n' >>"$rc"
    fi
    printf '%s\n' "$PATH_LINE" >>"$rc"
    say "added $wrap_dir to PATH in $rc; open a new terminal (or run: . $rc)"
}

# The PATH line out of every start file it can be in; the other lines stay.
# Written in place: a linked start file stays a link.
strip_path_line() {
    for rc in "$home/.zshrc" "$home/.bashrc" "$home/.profile"; do
        if [ -f "$rc" ] && grep -q -x -F "$PATH_LINE" "$rc"; then
            grep -v -x -F "$PATH_LINE" "$rc" >"$tmp/rc" || true
            cat "$tmp/rc" >"$rc"
            say "removed the PATH line of this script from $rc"
        fi
    done
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
        | awk -v mark="$HOST_MARKED" '
            host && index($0, "CCTG_HOST=") == 1 { host = 0; next }
            index($0, mark) == 1 { host = 1; next }
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
    strip_path_line
    if [ -f "$bin_dir/cctg.local-hub" ]; then
        say "left $exe: the local hub runs from it (install.sh --hub --local --uninstall removes the hub)"
        say "uninstalled; ~/.claude and ~/.claude.json were not touched"
        return 0
    fi
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

# The bot token, the group, the users and the proxy: given, kept in
# hub.env, or asked for. Sets $token, $chat_id, $users, $proxy and
# $new_secret (a shared secret when hub.env has none).
ask_hub_settings() {
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
    # The shared secret: the hub needs one while CCTG_SHARED_SECRET is on;
    # devices get their own through join codes and never see it.
    new_secret=
    if [ -z "$(value_of "$hub_env" CCTG_HUB_SECRET)" ]; then
        new_secret=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
    fi
}

# write_hub_env <header> <more managed keys, a|b> <their lines>: hub.env
# (600) with the settings of ask_hub_settings; lines of other keys stay.
write_hub_env() {
    keys="CCTG_BOT_TOKEN|CCTG_CHAT_ID|CCTG_ALLOWED_USER_IDS|CCTG_HUB_SECRET|HTTPS_PROXY|$2"
    old_umask=$(umask)
    umask 077
    {
        if [ -f "$hub_env" ]; then
            grep -v -E "^[[:space:]]*(export[[:space:]]+)?($keys)[[:space:]]*=" "$hub_env" \
                | grep -v -x -F "$PROXY_NONE" | grep -v -F "$LISTEN_MARK" || true
        else
            printf '%s\n' "$1"
        fi
        hub_line CCTG_BOT_TOKEN "$token"
        hub_line CCTG_CHAT_ID "$chat_id"
        hub_line CCTG_ALLOWED_USER_IDS "$users"
        hub_line CCTG_HUB_SECRET "$new_secret"
        hub_line HTTPS_PROXY "$proxy"
        [ -n "$proxy$(line_of "$hub_env" HTTPS_PROXY)" ] || printf '%s\n' "$PROXY_NONE"
        [ -z "$3" ] || printf '%s\n' "$3"
    } | put "$hub_env" 600
    umask "$old_umask"
    token=
}

# A self-signed certificate (EC P-256, 10 years) in $1 unless there is one;
# its sha256 -> $pin.
make_cert() {
    if [ ! -f "$1/cert.pem" ] || [ ! -f "$1/key.pem" ]; then
        say "making the hub certificate (self-signed, EC P-256, 10 years)"
        (
            cd "$1"
            # MSYS would turn /CN=... into a Windows path.
            MSYS_NO_PATHCONV=1 openssl ecparam -name prime256v1 -genkey -noout -out key.pem
            MSYS_NO_PATHCONV=1 openssl req -x509 -new -key key.pem -days 3650 \
                -subj /CN=cctg-hub -out cert.pem
        ) >/dev/null 2>&1 </dev/null || die "openssl could not make the certificate"
    fi
    fp=$(cd "$1" && openssl x509 -in cert.pem -noout -fingerprint -sha256 </dev/null) \
        || die "openssl cannot read $1/cert.pem"
    normalize_pin "$fp"
}

# hub_where <host> <agent port> <hook port> [<hook host>]: the client flags
# for a hub at <host>: --hub-host at the usual ports. An [IPv6] host is
# quoted: unquoted it is a glob pattern (zsh fails on it).
hub_where() {
    h=${4:-$1}
    q=
    qh=
    case $1 in \[*) q="'" ;; esac
    case $h in \[*) qh="'" ;; esac
    if [ "$h" = "$1" ] && [ "$2:$3" = "$AGENT_PORT:$HOOK_PORT" ]; then
        printf '%s' "--hub-host $q$1$q"
    else
        printf '%s' "--agent-addr $q$1:$2$q --hook-addr $qh$h:$3$qh"
    fi
}

# The host this machine reaches a listener (ip:port, empty: the default
# 127.0.0.1) at: 127.0.0.1 for 0.0.0.0, [::1] for [::] (on Windows it takes
# IPv6 only), else the address itself.
loopback_of() {
    case ${1%:*} in
        ''|0.0.0.0|127.*) printf '%s' 127.0.0.1 ;;
        \[::\]|\[::1\]) printf '%s' '[::1]' ;;
        *) printf '%s' "${1%:*}" ;;
    esac
}

# check_public_host <host> <error text>: a host name, an IPv4 address or an
# [IPv6] address in brackets; the hub takes nothing else in CCTG_PUBLIC_*.
check_public_host() {
    case $1 in
        \[*\])
            inner=${1#\[}
            inner=${inner%\]}
            case $inner in
                ''|*[!0-9A-Fa-f:.]*) die "$2" ;;
            esac
            ;;
        ''|*[!A-Za-z0-9._-]*) die "$2" ;;
    esac
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
    check_public_host "$public_host" "--public-host takes this server's host name, IPv4 address or [IPv6] address"
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

    ask_hub_settings

    # A proxy on the host's loopback needs the host network.
    compose_files=compose.yml
    case ${proxy:-$(value_of "$hub_env" HTTPS_PROXY)} in
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
    # How devices reach the hub, for /join (TASK-046).
    write_hub_env "# cctg hub settings, written by install.sh --hub (docs/remote-hub.md). Never commit." \
        "CCTG_PUBLIC_AGENT_ADDR|CCTG_PUBLIC_HOOK_ADDR" \
        "CCTG_PUBLIC_AGENT_ADDR=$public_host:$agent_port
CCTG_PUBLIC_HOOK_ADDR=$public_host:$hook_port"

    make_cert "$hub_dir/tls"
    if [ "$(id -u)" = 0 ]; then
        chown 10001 "$hub_dir/tls/key.pem" && chmod 600 "$hub_dir/tls/key.pem"
    else
        chmod 644 "$hub_dir/tls/key.pem"
        say "note: tls/key.pem is readable by every user here (as root it would belong to uid 10001 only)"
    fi

    say "starting the hub (docker compose in $hub_dir)"
    (cd "$hub_dir" && docker compose pull hub </dev/null)         || die "docker compose pull failed (the ghcr.io package must be public, or docker login ghcr.io)"
    started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    (cd "$hub_dir" && docker compose up -d --force-recreate hub </dev/null) || die "docker compose up failed"
    wait_hub

    where=$(hub_where "$public_host" "$agent_port" "$hook_port")
    # A one-time join code of the running hub (TASK-045).
    more="cd $hub_dir && docker compose exec hub cctg hub code"
    code=$(cd "$hub_dir" && docker compose exec -T hub cctg hub code </dev/null) \
        || die "the hub is up but gave no join code; ask it for one: $more"
    case $code in
        ????-????-????-????) ;;
        *) die "the hub is up but gave no join code; ask it for one: $more" ;;
    esac
    say "the hub is up. On a device with Claude Code run this line; its join code works once, for 10 minutes. For every next device a new code: /join in the group's General topic (or $more)"
    printf '\n%s\n\n' "curl -fsSL https://raw.githubusercontent.com/$REPO/$RELEASE/install.sh | sh -s -- $where --pin $pin --join $code"
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

# ------------------------------------------------- local hub (--hub --local)

# The local hub's files: $hub_dir, $hub_env, $hub_log, $state_dir and the
# autostart file of this system ($auto_file).
local_hub_paths() {
    hub_dir=${hub_dir:-$root/hub}
    [ "$os" != windows ] || hub_dir=$(cygpath -u "$hub_dir")
    hub_env=$hub_dir/hub.env
    hub_log=$hub_dir/hub.log
    state_dir=$hub_dir/state
    marker=$bin_dir/cctg.local-hub
    case $os in
        linux) auto_file=${XDG_CONFIG_HOME:-$home/.config}/systemd/user/$HUB_UNIT ;;
        macos) auto_file=$home/Library/LaunchAgents/$HUB_LABEL.plist ;;
        windows)
            auto_file=$hub_dir/start-hub.js
            run_key=${CCTG_INSTALL_RUN_KEY:-'HKCU\Software\Microsoft\Windows\CurrentVersion\Run'}
            wscript=$(cygpath -W)/System32/wscript.exe
            # The Run value; the paths of the hub are in the launcher.
            run_cmd="\"$(cygpath -w "$wscript")\" //B //Nologo \"$(cygpath -w "$auto_file")\""
            ;;
    esac
    # They go into a unit file, a plist or a command line unescaped.
    for p in "$(native "$hub_dir")" "$(native "$exe")"; do
        case $p in
            *[\"\$\`%\\\&\<\>]*) die "the path $p has a character the autostart entry cannot carry" ;;
        esac
    done
}

# user_listen <key>: the value of the user's own CCTG_*_LISTEN line in
# hub.env; empty when there is none or this script wrote it (LISTEN_MARK).
user_listen() {
    grep -q -x -F "$LISTEN_MARK $1" "$hub_env" 2>/dev/null || value_of "$hub_env" "$1"
}

# The local hub's listeners for $public_host: $agent_port and $hook_port,
# and for write_hub_env the keys to drop ($listen_keys, "|KEY...") and the
# lines to write ($listen_lines). The user's own CCTG_*_LISTEN lines stay.
# For other machines the rest listen on every address of the host's kind
# (a loopback host only in tests: no firewall question), marked to be
# chosen again on the next run.
listeners() {
    agent_listen=$(user_listen CCTG_AGENT_LISTEN)
    hook_listen=$(user_listen CCTG_HOOK_LISTEN)
    agent_port=${agent_listen##*:}
    hook_port=${hook_listen##*:}
    agent_port=${agent_port:-$AGENT_PORT}
    hook_port=${hook_port:-$HOOK_PORT}
    case $public_host in
        '') wild= ;;
        localhost|127.*) wild=127.0.0.1 ;;
        \[::1\]) wild='[::1]' ;;
        \[*) wild='[::]' ;;
        *) wild=0.0.0.0 ;;
    esac
    listen_keys=
    listen_lines=
    if [ -z "$agent_listen" ]; then
        listen_keys="$listen_keys|CCTG_AGENT_LISTEN"
        [ -z "$wild" ] || listen_lines="$LISTEN_MARK CCTG_AGENT_LISTEN
CCTG_AGENT_LISTEN=$wild:$agent_port"
    fi
    if [ -z "$hook_listen" ]; then
        listen_keys="$listen_keys|CCTG_HOOK_LISTEN"
        [ -z "$wild" ] || listen_lines="${listen_lines:+$listen_lines
}$LISTEN_MARK CCTG_HOOK_LISTEN
CCTG_HOOK_LISTEN=$wild:$hook_port"
    fi
}

# This machine's client flags: each listener of hub.env at the address it
# takes loopback connections on.
local_where() {
    hub_where "$(loopback_of "$(value_of "$hub_env" CCTG_AGENT_LISTEN)")" "$agent_port" "$hook_port" \
        "$(loopback_of "$(value_of "$hub_env" CCTG_HOOK_LISTEN)")"
}

# js_string <text>: a JScript string literal of <text> in ASCII (Windows
# Script Host reads a .js file in the ANSI code page). Windows only: od
# reads the UTF-16 units in the machine's (little-endian) byte order.
js_string() {
    printf '%s' "$1" | iconv -f UTF-8 -t UTF-16LE | od -An -v -tu2 | awk '
        BEGIN { printf "\"" }
        {
            for (i = 1; i <= NF; i++) {
                n = $i + 0
                if (n == 92) printf "\\\\"
                else if (n >= 32 && n < 127 && n != 34) printf "%c", n
                else printf "\\u%04x", n
            }
        }
        END { printf "\"" }'
}

# The autostart of this system is there to use, before anything is written.
check_autostart() {
    case $os in
        linux)
            if ! systemctl --user show-environment >/dev/null 2>&1 </dev/null; then
                die "--hub --local needs a systemd user manager (systemctl --user); without one run the hub with Docker (--hub) or start cctg supervise yourself (docs/poc.md)"
            fi
            ;;
        macos) command -v launchctl >/dev/null 2>&1 || die "--hub --local needs launchctl" ;;
        windows)
            [ -f "$wscript" ] || die "--hub --local needs Windows Script Host ($wscript) to start the hub without a window"
            command -v reg >/dev/null 2>&1 || die "--hub --local needs reg.exe"
            command -v iconv >/dev/null 2>&1 || die "--hub --local needs iconv (Git Bash has it)"
            # A Run value is a command line of at most 260 characters.
            n=$(printf '%s' "$run_cmd" | iconv -f UTF-8 -t UTF-16LE | wc -c)
            n=$((n / 2))
            [ "$n" -le 260 ] || die "the autostart command would be $n characters, Windows runs at most 260 from the Run key: choose a shorter --dir than $(native "$hub_dir")"
            ;;
    esac
}

setup_local_hub() {
    local_hub_paths
    check_autostart
    # Other machines: --public-host, or what hub.env says from before.
    kept=$(value_of "$hub_env" CCTG_PUBLIC_AGENT_ADDR)
    public_host=${public_host:-${kept%:*}}
    if [ -z "$public_host" ] && [ ! -f "$hub_env" ]; then
        ask "Host name or IP other machines reach this one at (empty: only this machine): "
        public_host=$answer
    fi
    [ -z "$public_host" ] || check_public_host "$public_host" "--public-host takes this machine's host name, IPv4 address or [IPv6] address"
    if [ -n "$public_host" ]; then
        command -v openssl >/dev/null 2>&1 || die "--public-host needs openssl for the hub certificate"
    fi
    mkdir -p "$hub_dir" "$state_dir"
    ask_hub_settings
    install_binary

    listeners
    more="CCTG_STATE_DIR=$(squote "$(native "$state_dir")")"
    pin=
    if [ -n "$public_host" ]; then
        mkdir -p "$hub_dir/tls"
        make_cert "$hub_dir/tls"
        chmod 600 "$hub_dir/tls/key.pem"
        more="$more
CCTG_TLS_CERT=$(squote "$(native "$hub_dir/tls/cert.pem")")
CCTG_TLS_KEY=$(squote "$(native "$hub_dir/tls/key.pem")")
CCTG_PUBLIC_AGENT_ADDR=$public_host:$agent_port
CCTG_PUBLIC_HOOK_ADDR=$public_host:$hook_port"
    fi
    [ -z "$listen_lines" ] || more="$more
$listen_lines"
    write_hub_env "# cctg hub on this machine, written by install.sh --hub --local. Never commit." \
        "CCTG_STATE_DIR|CCTG_TLS_CERT|CCTG_TLS_KEY|CCTG_PUBLIC_AGENT_ADDR|CCTG_PUBLIC_HOOK_ADDR$listen_keys" \
        "$more"

    : >"$marker"
    offset=$(log_size)
    start_local_hub
    wait_local_hub
    # The state directory of hub.env, as the hub has it (it runs with the
    # logon environment, not the installer's).
    code=$(unset CCTG_STATE_DIR; "$exe" hub --env-file "$(native "$hub_env")" code </dev/null) \
        || die "the hub is up but gave no join code; ask it: $exe hub --env-file $(native "$hub_env") code"
    raw_line="curl -fsSL https://raw.githubusercontent.com/$REPO/$RELEASE/install.sh | sh -s --"
    where=$(local_where)
    [ -z "$pin" ] || where="$where --pin $pin"
    say "the hub runs on this machine and starts at logon; its log: $(native "$hub_log")"
    [ "$os" != linux ] || say "note: it runs while you are logged in; to keep it running after logout: loginctl enable-linger $(id -un)"
    say "this machine's client: run this line (the code works once, for 10 minutes):"
    printf '\n%s\n\n' "$raw_line $where --join $code"
    if [ -n "$public_host" ]; then
        say "other machines: /join in the group's General topic gives a line with a new code (TLS, pin $pin)"
    else
        say "only this machine can use this hub; for other machines run this again with --public-host HOST"
    fi
}

log_size() {
    if [ -f "$hub_log" ]; then wc -c <"$hub_log" | tr -d ' '; else echo 0; fi
}

# Registers the autostart entry of this system and (re)starts the hub now
# on the binary just installed.
start_local_hub() {
    c=$(native "$exe")
    e=$(native "$hub_env")
    l=$(native "$hub_log")
    case $os in
        linux)
            mkdir -p "$(dirname "$auto_file")"
            put "$auto_file" 644 <<EOF
# cctg hub, written by install.sh --hub --local ($MARK).
[Unit]
Description=cctg hub (Telegram bridge for Claude Code)

[Service]
ExecStart="$c" supervise --env-file "$e" --log-file "$l"
Restart=on-failure
RestartSec=5
# The supervisor stops its hub itself (up to 30 s).
KillMode=mixed
TimeoutStopSec=40

[Install]
WantedBy=default.target
EOF
            systemctl --user daemon-reload </dev/null || die "systemctl --user daemon-reload failed"
            systemctl --user enable "$HUB_UNIT" </dev/null >/dev/null 2>&1 || die "systemctl --user enable $HUB_UNIT failed"
            systemctl --user restart "$HUB_UNIT" </dev/null || die "systemctl --user restart $HUB_UNIT failed"
            ;;
        macos)
            mkdir -p "$(dirname "$auto_file")"
            put "$auto_file" 644 <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- cctg hub, written by install.sh --hub --local ($MARK). -->
<plist version="1.0">
<dict>
  <key>Label</key><string>$HUB_LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$c</string>
    <string>supervise</string>
    <string>--env-file</string>
    <string>$e</string>
    <string>--log-file</string>
    <string>$l</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ExitTimeOut</key><integer>40</integer>
</dict>
</plist>
EOF
            uid=$(id -u)
            # Again from the file: a changed binary or plist takes effect.
            # The old supervisor must be gone first (a second one leaves).
            if launchctl print "gui/$uid/$HUB_LABEL" </dev/null >/dev/null 2>&1; then
                launchctl bootout "gui/$uid/$HUB_LABEL" </dev/null >/dev/null 2>&1 || true
                wait_stopped "stopping"
            fi
            launchctl bootstrap "gui/$uid" "$auto_file" </dev/null || die "launchctl bootstrap gui/$uid $auto_file failed"
            ;;
        windows)
            {
                cat <<'EOF'
// cctg hub at logon without a window, written by install.sh --hub --local
// (cctg-install). The hub's variables (CCTG_*, proxies) are the logon ones
// of the registry, also when the installer starts it from a shell that has
// others. Run(..., 0): hidden, not waited for.
var shell = new ActiveXObject("WScript.Shell");
var env = shell.Environment("Process");
function logon(name) {
    var kinds = ["Volatile", "User", "System"];
    for (var k = 0; k < kinds.length; k++) {
        var value = shell.Environment(kinds[k])(name);
        if (value) return shell.ExpandEnvironmentStrings(value);
    }
    return "";
}
var names = [];
for (var e = new Enumerator(env); !e.atEnd(); e.moveNext()) {
    var name = String(e.item()).split("=")[0];
    if (/^(CCTG_|(HTTPS?|ALL|NO)_PROXY$)/i.test(name)) names.push(name);
}
for (var i = 0; i < names.length; i++) {
    var value = logon(names[i]);
    if (value) env(names[i]) = value; else env.Remove(names[i]);
}
function q(s) { return '"' + s + '"'; }
EOF
                printf 'shell.Run(q(%s) + " supervise --env-file " + q(%s) + " --log-file " + q(%s), 0, false);\n' \
                    "$(js_string "$(cygpath -w "$exe")")" "$(js_string "$(cygpath -w "$hub_env")")" \
                    "$(js_string "$(cygpath -w "$hub_log")")"
            } | put "$auto_file" 644
            MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' reg add "$run_key" /v "$HUB_RUN_VALUE" /t REG_SZ \
                /d "$run_cmd" /f </dev/null >/dev/null || die "reg add $run_key failed"
            # A running supervisor restarts its hub on the new binary; a
            # second one started now leaves at once. The same command as at
            # logon.
            : >"$bin_dir/cctg.restart"
            MSYS_NO_PATHCONV=1 "$wscript" //B //Nologo "$(cygpath -w "$auto_file")" </dev/null \
                || die "wscript could not start the hub"
            ;;
    esac
}

# The hub's own start checks end in "hub started, polling"; a failed one
# ends the hub with "Error: ..." (the supervisor then tries again). Only
# hubs the supervisor started after $offset count: an older hub still in
# its start checks writes its own "Error: ..." while it is being stopped.
# A hub is marked by "hub starting", which the supervisor writes before it
# starts the hub; its "hub started pid=" comes after the start and can come
# after the hub's own lines (TASK-071). A supervisor of an older release
# writes only the latter.
wait_local_hub() {
    i=0
    logs=
    while [ $i -lt 60 ]; do
        sleep 1
        [ "$(log_size)" -ge "$offset" ] || offset=0
        logs=$(tail -c "+$((offset + 1))" "$hub_log" 2>/dev/null || true)
        case $logs in
            *"hub starting"*) hub_lines=${logs#*"hub starting"} ;;
            *"hub started pid="*) hub_lines=${logs#*"hub started pid="} ;;
            *) hub_lines= ;;
        esac
        case $hub_lines in
            *"hub started, polling"*) say "hub started: bot and group checked"; return 0 ;;
            *"Error: "*) break ;;
        esac
        i=$((i + 1))
    done
    if [ -z "$logs" ]; then
        die "no hub wrote to $(native "$hub_log") in 60 s: is a cctg supervise started by hand, or by an older release, running from $(native "$bin_dir")? Stop it, then run this again"
    fi
    printf '%s\n' "$logs" | tail -n 20
    die "the hub did not start; its log is above ($(native "$hub_log"))"
}

# wait_stopped <line>: a supervisor that logs <line> within 3 s is waited
# for until it logs "supervisor stopped" (45 s at most).
wait_stopped() {
    i=0
    while [ $i -lt 45 ]; do
        logs=$(tail -c "+$((offset + 1))" "$hub_log" 2>/dev/null || true)
        case $logs in
            *"supervisor stopped"*) say "the hub stopped"; return 0 ;;
            *"$1"*) ;;
            *) [ $i -lt 3 ] || { say "no running supervisor answered (none ran, or one of an older release: stop that one by hand)"; return 0; } ;;
        esac
        sleep 1
        i=$((i + 1))
    done
    say "the hub did not say it stopped; see $(native "$hub_log")"
}

uninstall_local_hub() {
    local_hub_paths
    [ -f "$hub_env" ] || die "no hub set up by this script in $hub_dir"
    offset=$(log_size)
    case $os in
        linux)
            # Waits until the supervisor stopped its hub.
            systemctl --user disable --now "$HUB_UNIT" </dev/null >/dev/null 2>&1 || true
            remove "$auto_file"
            systemctl --user daemon-reload </dev/null >/dev/null 2>&1 || true
            wait_stopped "stopping"
            ;;
        macos)
            launchctl bootout "gui/$(id -u)/$HUB_LABEL" </dev/null >/dev/null 2>&1 || true
            remove "$auto_file"
            wait_stopped "stopping"
            ;;
        windows)
            MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' reg delete "$run_key" /v "$HUB_RUN_VALUE" /f </dev/null >/dev/null 2>&1 || true
            remove "$auto_file"
            # No service manager: the supervisor stops on cctg.stop.
            [ ! -d "$bin_dir" ] || : >"$bin_dir/cctg.stop"
            wait_stopped "stopping: cctg.stop"
            rm -f "$bin_dir/cctg.stop"
            ;;
    esac
    remove "$marker"
    say "removed the autostart; $hub_dir keeps hub.env (token, secret), tls/, state/ and the log: delete them by hand if you want"
}

main "$@"
