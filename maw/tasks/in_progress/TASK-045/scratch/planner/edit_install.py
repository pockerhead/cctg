import sys
root = sys.argv[1]


def patch(rel, pairs):
    p = root + '/' + rel
    s = open(p, encoding='utf-8').read()
    for old, new, *count in pairs:
        n = s.count(old)
        assert n == (count[0] if count else 1), (rel, old[:80], n)
        s = s.replace(old, new)
    open(p, 'w', encoding='utf-8', newline='\n').write(s)


patch('install.sh', [
("""# It never writes ~/.claude/settings.json or ~/.claude.json and never prints
# the hub secret (--hub prints it once, inside the client install line).""",
 """# It never writes ~/.claude/settings.json or ~/.claude.json and never prints
# a secret: with --join the device trades a one-time code for its own secret
# (cctg join writes it into device.env); --hub prints such a code, inside
# the client install line."""),
("""secret_file=
host=""", """secret_file=
join_code=${CCTG_JOIN_CODE:-}
host="""),
("""        --secret-file) need_value "$@"; secret_file=$2; shift 2 ;;""",
 """        --secret-file) need_value "$@"; secret_file=$2; shift 2 ;;
        --join) need_value "$@"; join_code=$2; shift 2 ;;"""),
("""install_binary
write_device_env
write_claude_files""", """install_binary
write_device_env
[ -z "$join_code" ] || join_hub
write_claude_files"""),
("""  --secret-file FILE    read the hub secret from the first line of FILE
                        (or set CCTG_HUB_SECRET; otherwise it is asked for)""",
 """  --join CODE           a one-time join code of the hub (cctg hub code on
                        the hub, /devices in Telegram): this device gets
                        its own secret (or set CCTG_JOIN_CODE)
  --secret-file FILE    read the hub's shared secret from the first line of
                        FILE (or set CCTG_HUB_SECRET; without a join code
                        or a secret the code is asked for)"""),
("""    secret=${CCTG_HUB_SECRET:-}
    # Nothing started from here gets it: cctg doctor must check the file,
    # as the sessions will.
    unset CCTG_HUB_SECRET
    if [ -n "$secret" ]; then
        :
    elif [ -n "$secret_file" ]; then""", """    secret=${CCTG_HUB_SECRET:-}
    # Nothing started from here gets it: cctg doctor must check the file,
    # as the sessions will.
    unset CCTG_HUB_SECRET CCTG_JOIN_CODE
    if [ -n "$join_code" ]; then
        # The device's own secret comes from the hub (join_hub).
        secret=
        check_code
    elif [ -n "$secret" ]; then
        :
    elif [ -n "$secret_file" ]; then"""),
("""    elif [ -z "$(old_line CCTG_HUB_SECRET)" ]; then
        interactive || die "no hub secret: set CCTG_HUB_SECRET or use --secret-file"
        read_hidden "Hub secret (CCTG_HUB_SECRET of the hub, not shown): " \\
            "set CCTG_HUB_SECRET or use --secret-file"
        secret=$hidden
        hidden=
        [ -n "$secret" ] || die "no hub secret typed"
    fi""", """    elif [ -z "$(old_line CCTG_HUB_SECRET)" ]; then
        interactive || die "no join code and no hub secret: use --join CODE (or CCTG_HUB_SECRET, --secret-file)"
        ask "Join code from the hub (empty: type the hub's shared secret instead): "
        join_code=$answer
        if [ -n "$join_code" ]; then
            check_code
        else
            read_hidden "Hub secret (CCTG_HUB_SECRET of the hub, not shown): " \\
                "use --join CODE, set CCTG_HUB_SECRET or use --secret-file"
            secret=$hidden
            hidden=
            [ -n "$secret" ] || die "no hub secret typed"
        fi
    fi"""),
("""check_host() {""", """# XXXX-XXXX-XXXX-XXXX as the hub prints it; the hub checks the rest.
check_code() {
    case $join_code in
        *[!A-Za-z0-9\\ -]*) die "a join code has letters, digits and dashes only" ;;
    esac
}

# The device's own secret for the join code: cctg join asks the hub (the
# address and pin just written) and puts the secret into device.env; it
# never prints it. A refused code leaves device.env as it was.
join_hub() {
    say "joining the hub with the code"
    CCTG_JOIN_CODE=$join_code "$exe" join </dev/null \\
        || die "the hub did not enroll this device (above); run this again with a new join code"
    join_code=
}

check_host() {"""),
("""    hub_secret=$(value_of "$hub_env" CCTG_HUB_SECRET)
    new_secret=
    if [ -z "$hub_secret" ]; then
        new_secret=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \\n')
        hub_secret=$new_secret
    fi""", """    # The shared secret: the hub needs one while CCTG_SHARED_SECRET is on;
    # devices get their own through join codes and never see it.
    new_secret=
    if [ -z "$(value_of "$hub_env" CCTG_HUB_SECRET)" ]; then
        new_secret=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \\n')
    fi"""),
("""    if [ "$agent_port:$hook_port" = "$AGENT_PORT:$HOOK_PORT" ]; then
        where="--hub-host $public_host"
    else
        where="--agent-addr $public_host:$agent_port --hook-addr $public_host:$hook_port"
    fi
    say "the hub is up. On each device with Claude Code run (the line carries the hub secret: only your own machines, and clear it from chat history):"
    printf '\\n%s\\n\\n' "curl -fsSL https://raw.githubusercontent.com/$REPO/$RELEASE/install.sh | CCTG_HUB_SECRET=$(squote "$hub_secret") sh -s -- $where --pin $pin"
    hub_secret=
}""", """    if [ "$agent_port:$hook_port" = "$AGENT_PORT:$HOOK_PORT" ]; then
        where="--hub-host $public_host"
    else
        where="--agent-addr $public_host:$agent_port --hook-addr $public_host:$hook_port"
    fi
    # A one-time join code of the running hub (TASK-045).
    more="cd $hub_dir && docker compose exec hub cctg hub code"
    code=$(cd "$hub_dir" && docker compose exec -T hub cctg hub code </dev/null) \\
        || die "the hub is up but gave no join code; ask it for one: $more"
    case $code in
        ????-????-????-????) ;;
        *) die "the hub is up but gave no join code; ask it for one: $more" ;;
    esac
    say "the hub is up. On a device with Claude Code run this line; its join code works once, for 10 minutes. For every next device a new code: $more (or /devices in Telegram says how)"
    printf '\\n%s\\n\\n' "curl -fsSL https://raw.githubusercontent.com/$REPO/$RELEASE/install.sh | sh -s -- $where --pin $pin --join $code"
}"""),
])
print('ok')
