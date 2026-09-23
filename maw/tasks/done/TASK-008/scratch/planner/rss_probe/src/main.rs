//! Idle-memory probe for TASK-008. Does what `cctg hub` does before polling
//! (real getMe + getChatMember + rights check over TLS, scheduler running),
//! plus getForumTopicIconStickers, then idles. No getUpdates: two updates are
//! pending in the real bot and must not be confirmed. Prints no ids or token.
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cctg::hub::api::BotApi;
use cctg::hub::config::Config;
use cctg::hub::scheduler::{BucketConfig, Scheduler};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load(Some(Path::new("C:/Users/user/dev/cctg/.env")))?;
    let api = Arc::new(BotApi::new(&config.token, config.chat_id)?);
    let me = api.get_me().await?;
    let member = api.get_chat_member(me.id).await?;
    cctg::hub::check_topic_rights(&member)?;
    let stickers = api.get_forum_topic_icon_stickers().await?;
    eprintln!(
        "status={} can_manage_topics={} can_delete_messages={} stickers={} with_custom_emoji_id={}",
        member.status,
        member.can_manage_topics,
        member.can_delete_messages,
        stickers.len(),
        stickers.iter().filter(|s| s.custom_emoji_id.is_some()).count()
    );
    let (scheduler, outbox) = Scheduler::new(api.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());
    eprintln!("idle");
    tokio::time::sleep(Duration::from_secs(45)).await;
    drop(outbox);
    Ok(())
}
