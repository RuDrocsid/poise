//! Contains all code to dispatch incoming events onto framework commands

mod common;
mod permissions;
mod prefix;
mod slash;

use std::collections::HashMap;
use std::sync::Arc;
use arc_swap::Guard;
pub use common::*;
pub use prefix::*;
pub use slash::*;

use crate::{serenity_prelude as serenity, CommandOverride, CommandStorage};

/// A view into data stored by [`crate::Framework`]
pub struct FrameworkContext<'a, U, E> {
    /// Serenity's context
    pub serenity_context: &'a serenity::Context,
    /// User ID of this bot, available through serenity_context if cache is enabled.
    #[cfg(not(feature = "cache"))]
    pub bot_id: serenity::UserId,
    /// Framework configuration
    pub options: &'a crate::FrameworkOptions<U, E>,
    /// Serenity shard manager. Can be used for example to shutdown the bot
    pub shard_manager: &'a std::sync::Arc<serenity::ShardManager>,
    // deliberately not non exhaustive because you need to create FrameworkContext from scratch
    // to run your own event loop
}
impl<U, E> Copy for FrameworkContext<'_, U, E> {}
impl<U, E> Clone for FrameworkContext<'_, U, E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a, U: Send + Sync + 'static, E> FrameworkContext<'a, U, E> {
    /// Returns the user ID of the bot.
    pub fn bot_id(&self) -> serenity::UserId {
        #[cfg(feature = "cache")]
        let bot_id = self.serenity_context.cache.current_user().id;
        #[cfg(not(feature = "cache"))]
        let bot_id = self.bot_id;

        bot_id
    }

    /// Returns the stored framework options, including commands.
    ///
    /// This function exists for API compatiblity with [`crate::Framework`]. On this type, you can
    /// also just access the public `options` field.
    pub fn options(&self) -> &'a crate::FrameworkOptions<U, E> {
        self.options
    }

    /// Returns the serenity's client shard manager.
    ///
    /// This function exists for API compatiblity with [`crate::Framework`]. On this type, you can
    /// also just access the public `shard_manager` field.
    pub fn shard_manager(&self) -> std::sync::Arc<serenity::ShardManager> {
        self.shard_manager.clone()
    }

    /// Retrieves user data
    pub fn user_data(&self) -> std::sync::Arc<U> {
        self.serenity_context.data::<U>()
    }
}

async fn get_guild_commands<'a, U: Send + Sync, E>(
    framework: &'a crate::FrameworkContext<'a, U, E>,
    guild_id: &Option<serenity::GuildId>,
) -> Option<(Guard<Arc<CommandStorage<U, E>>>, Guard<Arc<HashMap<String, CommandOverride>>>)> {
    if let Some(gid) = guild_id {
        let lock = framework.options.guild_commands.read().await;
        return lock.get(gid).map(|s| (s.0.deref_owned(), s.1.deref_owned()));
    }
    None
}
// Guard<Arc<CommandStorage<U, E>>>
fn deref_optional_tuple<T, U>(lock: &Option<(T, U)>) -> Option<(&T, &U)> {
    match lock {
        None => None,
        Some((t, u)) => Some((t, u)),
    }
}

/// Central event handling function of this library
pub async fn dispatch_event<U: Send + Sync + 'static, E>(
    framework: crate::FrameworkContext<'_, U, E>,
    event: &serenity::FullEvent,
) {
    match event {
        serenity::FullEvent::Message { new_message } => {
            let invocation_data = tokio::sync::Mutex::new(Box::new(()) as _);
            let mut parent_commands = Vec::new();
            let commands_lock = framework.options().commands.deref_owned();
            let guild_commands_opt = get_guild_commands(&framework, &new_message.guild_id).await;
            if let Err(error) = prefix::dispatch_message(
                framework,
                &commands_lock,
                deref_optional_tuple(&guild_commands_opt),
                new_message,
                crate::MessageDispatchTrigger::MessageCreate,
                &invocation_data,
                &mut parent_commands,
            )
            .await
            {
                error.handle(framework.options).await;
            }
        }
        serenity::FullEvent::MessageUpdate { event, .. } => {
            if let Some(edit_tracker) = &framework.options.prefix_options.edit_tracker {
                let result = edit_tracker.write().unwrap().process_message_update(
                    event,
                    framework
                        .options()
                        .prefix_options
                        .ignore_edits_if_not_yet_responded,
                );

                if let Some(previously_tracked) = result {
                    let invocation_data = tokio::sync::Mutex::new(Box::new(()) as _);
                    let mut parent_commands = Vec::new();
                    let commands_lock = framework.options().commands.deref_owned();
                    let guild_commands_opt = get_guild_commands(&framework, &event.message.guild_id).await;
                    let trigger = match previously_tracked {
                        true => crate::MessageDispatchTrigger::MessageEdit,
                        false => crate::MessageDispatchTrigger::MessageEditFromInvalid,
                    };
                    if let Err(error) = prefix::dispatch_message(
                        framework,
                        &commands_lock,
                        deref_optional_tuple(&guild_commands_opt),
                        &event.message,
                        trigger,
                        &invocation_data,
                        &mut parent_commands,
                    )
                    .await
                    {
                        error.handle(framework.options).await;
                    }
                }
            }
        }
        serenity::FullEvent::MessageDelete {
            deleted_message_id, ..
        } => {
            if let Some(edit_tracker) = &framework.options.prefix_options.edit_tracker {
                let bot_response = edit_tracker
                    .write()
                    .unwrap()
                    .process_message_delete(*deleted_message_id);
                if let Some(bot_response) = bot_response {
                    if let Err(e) = bot_response
                        .delete(&framework.serenity_context.http, None)
                        .await
                    {
                        tracing::warn!("failed to delete bot response: {}", e);
                    }
                }
            }
        }
        serenity::FullEvent::InteractionCreate {
            interaction: serenity::Interaction::Command(interaction),
        } => {
            let invocation_data = tokio::sync::Mutex::new(Box::new(()) as _);
            let mut parent_commands = Vec::new();
            let commands_lock = framework.options().commands.deref_owned();
            let guild_commands_opt = get_guild_commands(&framework, &interaction.guild_id).await;
            if let Err(error) = slash::dispatch_interaction(
                framework,
                &commands_lock,
                deref_optional_tuple(&guild_commands_opt),
                interaction,
                &std::sync::atomic::AtomicBool::new(false),
                &invocation_data,
                &interaction.data.options(),
                &mut parent_commands,
            )
            .await
            {
                error.handle(framework.options).await;
            }
        }
        serenity::FullEvent::InteractionCreate {
            interaction: serenity::Interaction::Autocomplete(interaction),
        } => {
            let invocation_data = tokio::sync::Mutex::new(Box::new(()) as _);
            let mut parent_commands = Vec::new();
            let commands_lock = framework.options().commands.deref_owned();
            let guild_commands_opt = get_guild_commands(&framework, &interaction.guild_id).await;
            if let Err(error) = slash::dispatch_autocomplete(
                framework,
                &commands_lock,
                deref_optional_tuple(&guild_commands_opt),
                interaction,
                &std::sync::atomic::AtomicBool::new(false),
                &invocation_data,
                &interaction.data.options(),
                &mut parent_commands,
            )
            .await
            {
                error.handle(framework.options).await;
            }
        }
        _ => {}
    }

    // Do this after the framework's Ready handling, so that get_user_data() doesnt
    // potentially block infinitely
    if let Err(error) = (framework.options.event_handler)(framework, event).await {
        let error = crate::FrameworkError::EventHandler {
            error,
            event,
            framework,
        };
        (framework.options.on_error)(error).await;
    }
}
