//! `cctg hub`: Telegram side of the bridge.

pub mod api;
pub mod config;
pub mod scheduler;
pub mod updates;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tracing::{info, warn};

use api::{BotApi, ChatMember};
use config::Config;
use scheduler::{BucketConfig, Scheduler};
use updates::Routed;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RightsError {
    #[error(
        "the bot is not an administrator of the configured chat (status: {0}); \
         promote it and grant the \"Manage Topics\" right"
    )]
    NotAdmin(String),
    #[error(
        "the bot is an administrator without the \"Manage Topics\" right \
         (can_manage_topics); grant it in the group admin settings"
    )]
    NoManageTopics,
}

/// Startup check: the hub cannot create topics without `can_manage_topics`.
pub fn check_topic_rights(member: &ChatMember) -> Result<(), RightsError> {
    match member.status.as_str() {
        "creator" => Ok(()),
        "administrator" if member.can_manage_topics => Ok(()),
        "administrator" => Err(RightsError::NoManageTopics),
        other => Err(RightsError::NotAdmin(other.to_owned())),
    }
}

pub async fn run(env_file: Option<&Path>) -> anyhow::Result<()> {
    let config = Config::load(env_file)?;
    let api = Arc::new(BotApi::new(&config.token, config.chat_id)?);

    let me = api
        .get_me()
        .await
        .context("getMe failed; check CCTG_BOT_TOKEN")?;
    let member = api.get_chat_member(me.id).await.context(
        "getChatMember for the bot failed; check CCTG_CHAT_ID and that the bot is in the group",
    )?;
    check_topic_rights(&member)?;
    if member.status == "administrator" && !member.can_delete_messages {
        warn!("the bot lacks can_delete_messages; forum service messages will stay visible");
    }
    info!(
        bot = me.username.as_deref().unwrap_or("?"),
        "hub started, polling"
    );

    let (scheduler, outbox) = Scheduler::new(api.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());

    updates::poll(&api, &config.allowlist, |routed| {
        // Handlers arrive with TASK-009/011; the outbox is kept alive for them.
        let _ = &outbox;
        match routed {
            Routed::Input(input) => info!(thread = ?input.thread_id, "inbound message"),
            Routed::Callback(_) => info!("inbound button press"),
            Routed::Service(_) | Routed::Ignored(_) => {}
        }
    })
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(status: &str, topics: bool) -> ChatMember {
        ChatMember {
            status: status.to_owned(),
            can_manage_topics: topics,
            can_delete_messages: true,
        }
    }

    #[test]
    fn missing_manage_topics_is_a_startup_error() {
        assert_eq!(check_topic_rights(&member("administrator", true)), Ok(()));
        assert_eq!(check_topic_rights(&member("creator", false)), Ok(()));
        assert_eq!(
            check_topic_rights(&member("administrator", false)),
            Err(RightsError::NoManageTopics)
        );
        for status in ["member", "restricted", "left", "kicked", ""] {
            assert_eq!(
                check_topic_rights(&member(status, false)),
                Err(RightsError::NotAdmin(status.to_owned()))
            );
        }
        let message = RightsError::NoManageTopics.to_string();
        assert!(message.contains("can_manage_topics"));
    }
}
