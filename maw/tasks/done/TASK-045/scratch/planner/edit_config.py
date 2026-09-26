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


patch('crates/cctg/src/hub/config.rs', [
("""/// Shared secret of agents and hooks; required by `cctg hub`.
pub const SECRET_VAR: &str = "CCTG_HUB_SECRET";""",
"""/// Shared secret of agents and hooks; required by `cctg hub` while
/// [`SHARED_VAR`] is on.
pub const SECRET_VAR: &str = "CCTG_HUB_SECRET";
/// Optional, `on` (default) or `off`: whether the hub still takes
/// [`SECRET_VAR`] from agents and hooks. Off once every device has its own
/// secret (TASK-045, `cctg join`).
pub const SHARED_VAR: &str = "CCTG_SHARED_SECRET";"""),
("""    #[error("{SECRET_VAR} is invalid: {0}")]
    Secret(SecretError),""",
"""    #[error("{SECRET_VAR} is invalid: {0}")]
    Secret(SecretError),
    #[error("{SHARED_VAR} must be on or off")]
    Shared,"""),
("""    /// `None` when `CCTG_HUB_SECRET` is unset; `cctg hub` refuses to start then.
    pub hub_secret: Option<Secret>,""",
"""    /// `None` when `CCTG_HUB_SECRET` is unset; `cctg hub` refuses to start
    /// then, unless `shared_secret` is off.
    pub hub_secret: Option<Secret>,
    /// [`SHARED_VAR`]: agents and hooks may use `hub_secret`.
    pub shared_secret: bool,"""),
("""        let hub_secret = optional(SECRET_VAR)
            .map(|value| Secret::parse(&value).map_err(ConfigError::Secret))
            .transpose()?;""",
"""        let hub_secret = optional(SECRET_VAR)
            .map(|value| Secret::parse(&value).map_err(ConfigError::Secret))
            .transpose()?;
        let shared_secret = match optional(SHARED_VAR).map(|value| value.to_ascii_lowercase()) {
            None => true,
            Some(value) if value == "on" => true,
            Some(value) if value == "off" => false,
            Some(_) => return Err(ConfigError::Shared),
        };"""),
("""            state_dir,
            hub_secret,
            agent_listen,""",
"""            state_dir,
            hub_secret,
            shared_secret,
            agent_listen,"""),
("""    fn a_bad_hub_secret_is_named_but_not_echoed() {""",
"""    fn the_shared_secret_is_on_unless_turned_off() {
        let base = [
            (TOKEN_VAR, "123:abc"),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "1"),
        ];
        let with = |extra: &[(&'static str, &'static str)]| {
            let pairs: Vec<_> = base.iter().chain(extra).copied().collect();
            Config::from_vars(vars(&pairs))
        };
        assert!(with(&[]).unwrap().shared_secret);
        assert!(with(&[(SHARED_VAR, " On ")]).unwrap().shared_secret);
        assert!(!with(&[(SHARED_VAR, "OFF")]).unwrap().shared_secret);
        let error = with(&[(SHARED_VAR, "maybe-secret-value")]).unwrap_err();
        assert_eq!(error, ConfigError::Shared);
        assert!(!error.to_string().contains("maybe-secret-value"));
    }

    #[test]
    fn a_bad_hub_secret_is_named_but_not_echoed() {"""),
])
print('ok')
