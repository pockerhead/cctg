# Reviewer-2 changes to mod.rs, sessions.rs, tests/slots_logs.rs (run once).
import os
HERE = os.path.dirname(os.path.abspath(__file__))
CCTG = os.path.join(HERE, 'ws', 'crates', 'cctg')


def edit(rel, pairs):
    p = os.path.join(CCTG, rel)
    s = open(p, encoding='utf-8').read()
    for old, new in pairs:
        assert s.count(old) == 1, (rel, old[:100])
        s = s.replace(old, new, 1)
    open(p, 'w', encoding='utf-8', newline='\n').write(s)


edit('src/hub/mod.rs', [
    ('''use std::collections::HashSet;
use std::path::Path;''', '''use std::path::Path;'''),
    ('''use api::{BotApi, ChatMember};''', '''use api::{ApiError, BotApi, ChatMember, Sticker};'''),
    ('''/// Default icons that Telegram does not offer are dropped with a warning; a
/// failed lookup keeps the defaults.
async fn checked_icons(api: &BotApi) -> Icons {
    let mut icons = Icons::default();
    match api.get_forum_topic_icon_stickers().await {
        Ok(stickers) => {
            let offered: HashSet<String> = stickers
                .into_iter()
                .filter_map(|sticker| sticker.custom_emoji_id)
                .collect();
            for state in icons.keep_valid(&offered) {
                warn!(
                    state,
                    "topic icon is not offered by Telegram; that state keeps the current icon"
                );
            }
        }
        Err(error) => warn!(%error, "getForumTopicIconStickers failed; using the default icons"),
    }
    icons
}''', '''/// Topic icons from `getForumTopicIconStickers`. A failed lookup stops the
/// start: an icon id Telegram did not offer is never sent. A preferred icon
/// that is not offered is replaced from the offered set with a warning.
fn checked_icons(lookup: Result<Vec<Sticker>, ApiError>) -> anyhow::Result<Icons> {
    let stickers = lookup.context("getForumTopicIconStickers failed; topic icons must come from it")?;
    let (icons, substituted) =
        Icons::from_offered(stickers.into_iter().filter_map(|sticker| sticker.custom_emoji_id))?;
    for state in substituted {
        warn!(
            state,
            "preferred topic icon is not offered by Telegram; another offered icon stands in"
        );
    }
    Ok(icons)
}'''),
    ('''    let icons = checked_icons(&api).await;''', '''    let icons = checked_icons(api.get_forum_topic_icon_stickers().await)?;'''),
    ('''    use api::{ApiError, Message};''', '''    use api::Message;'''),
    ('''    #[test]
    fn missing_manage_topics_is_a_startup_error() {''', '''    #[test]
    fn icons_come_only_from_a_successful_lookup() {
        let failed = checked_icons(Err(ApiError::Telegram {
            code: 500,
            description: "Internal Server Error".to_owned(),
        }));
        assert!(failed.is_err());
        let sticker = |id: &str| Sticker {
            custom_emoji_id: Some(id.to_owned()),
        };
        let offered = vec![sticker("4"), sticker("3"), sticker("2"), sticker("1")];
        let icons = checked_icons(Ok(offered)).unwrap();
        assert_eq!(icons.alive.as_deref(), Some("1"));
        assert_eq!(icons.no_channel.as_deref(), Some("4"));
        assert!(checked_icons(Ok(vec![sticker("1")])).is_err());
    }

    #[test]
    fn missing_manage_topics_is_a_startup_error() {'''),
])

edit('src/hub/sessions.rs', [
    ('''    /// The topic's session is known only from its agent: no transcript path.''',
     '''    /// The topic's session was announced without a transcript path.'''),
])

edit('tests/slots_logs.rs', [
    ('''    let options = Options {
        grace: Duration::ZERO,
        hook_wait: Duration::from_millis(100),
        ..Options::default()
    };''', '''    let options = Options {
        grace: Duration::ZERO,
        ..Options::default()
    };'''),
    ('''    // An agent of a session nobody announced, adopted after the wait.''',
     '''    // An agent of a session nobody announced: it waits, no topic.'''),
])
print('ok')
