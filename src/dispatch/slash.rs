//! Dispatches interactions onto framework commands

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use arc_swap::{ArcSwap, Guard};
use serenity::all::CommandType;
use crate::{serenity_prelude as serenity, CommandOverride, CommandStorage, ContextMenuCommandAction};

/// Check if the interaction with the given name and arguments matches any framework command
fn find_matching_command<'a, 'b, U, E>(
    interaction_name: &str,
    command_kind: serenity::CommandType,
    interaction_options: &'b [serenity::ResolvedOption<'b>],
    commands: &'a CommandStorage<U, E>, 
    overrides: &HashMap<String, CommandOverride>,
    parent_commands: &mut Vec<&'a crate::Command<U, E>>,
) -> Option<(&'a crate::Command<U, E>, &'b [serenity::ResolvedOption<'b>])> {
    let default_override = CommandOverride::default();

    if matches!(command_kind, CommandType::PrimaryEntryPoint) {
        return None;
    }

    // search for context menu commands recursively; we can not split the name in this case
    if matches!(command_kind, CommandType::User) || matches!(command_kind, CommandType::Message) { 
        let (user, message) = commands.get_context_commands();
        if matches!(command_kind, CommandType::User) {
            for command in user {
                if interaction_name == command.context_menu_name.as_ref().unwrap_or(&command.name) {
                    return Some((command, interaction_options));
                }
            }
            return None;
        } 
        
        if matches!(command_kind, CommandType::Message) {
            for command in message {
                if interaction_name == command.context_menu_name.as_ref().unwrap_or(&command.name) {
                    return Some((command, interaction_options));
                }
            }
        }
    }   
    
    commands.iter().find_map(|cmd| {
        let command_override = cmd
            .command_id
            .as_ref()
            .and_then(|id| overrides.get(id))
            .unwrap_or(&default_override);
        if command_override.disabled || command_override.group_disabled {
            return None;
        }
        
        // do not match a slash command, when context menu command was executed and vice versa
        if cmd.slash_action.is_none() || interaction_name != cmd.name { 
            return None;
        }

        if let Some((sub_name, sub_interaction)) =
            interaction_options
                .iter()
                .find_map(|option| match &option.value {
                    serenity::ResolvedValue::SubCommand(o)
                    | serenity::ResolvedValue::SubCommandGroup(o) => Some((&option.name, o)),
                    _ => None,
                })
        {
            parent_commands.push(cmd);
            find_matching_command(sub_name, command_kind, sub_interaction, &cmd.subcommands, overrides, parent_commands)
        } else {
            Some((cmd, interaction_options))
        }
    })
}

/// Parses an `Interaction` into a [`crate::ApplicationContext`] using some context data.
///
/// After this, the [`crate::ApplicationContext`] should be passed into [`run_command`] or
/// [`run_autocomplete`].
#[allow(clippy::too_many_arguments)] // We need to pass them all in to create Context.
fn extract_command<'a, U, E>(
    framework: crate::FrameworkContext<'a, U, E>,
    global_commands: &'a Guard<Arc<CommandStorage<U, E>>>,
    guild_commands: Option<(&'a Guard<Arc<CommandStorage<U, E>>>, &'a Guard<Arc<HashMap<String, CommandOverride>>>)>,
    interaction: &'a serenity::CommandInteraction,
    interaction_type: crate::CommandInteractionType,
    has_sent_initial_response: &'a std::sync::atomic::AtomicBool,
    invocation_data: &'a tokio::sync::Mutex<Box<dyn std::any::Any + Send + Sync>>,
    options: &'a [serenity::ResolvedOption<'a>],
    parent_commands: &'a mut Vec<&'a crate::Command<U, E>>,
) -> Result<crate::ApplicationContext<'a, U, E>, crate::FrameworkError<'a, U, E>> {
    let empty_map = ArcSwap::new(Arc::new(HashMap::new())).load();

    // if the called command is registered in a guild, search in guild commands; 
    // else search in global commands
    let cmds = if interaction.data.guild_id.is_some() { 
        guild_commands
    } else {
        Some((global_commands, &empty_map))
    };
    
    let (command, leaf_interaction_options) = cmds
        .map(|(cmds, overrides)| {
            find_matching_command(
                &interaction.data.name,
                interaction.data.kind,
                options,
                cmds.deref(),
                overrides,
                parent_commands,
            )
        })
        .flatten()
        .ok_or(crate::FrameworkError::UnknownInteraction {
            framework,
            interaction,
    })?;

    Ok(crate::ApplicationContext {
        framework,
        interaction,
        interaction_type,
        args: leaf_interaction_options,
        command,
        parent_commands,
        has_sent_initial_response,
        invocation_data,
        global_commands: Some(global_commands),
        guild_commands,
        __non_exhaustive: (),
    })
}

/// Given an interaction, finds the matching framework command and checks if the user is allowed access
#[allow(clippy::too_many_arguments)] // We need to pass them all in to create Context.
pub async fn extract_command_and_run_checks<'a, U: Send + Sync + 'static, E>(
    framework: crate::FrameworkContext<'a, U, E>,
    global_commands: &'a Guard<Arc<CommandStorage<U, E>>>,
    guild_commands: Option<(&'a Guard<Arc<CommandStorage<U, E>>>, &'a Guard<Arc<HashMap<String, CommandOverride>>>)>,
    interaction: &'a serenity::CommandInteraction,
    interaction_type: crate::CommandInteractionType,
    has_sent_initial_response: &'a std::sync::atomic::AtomicBool,
    invocation_data: &'a tokio::sync::Mutex<Box<dyn std::any::Any + Send + Sync>>,
    options: &'a [serenity::ResolvedOption<'a>],
    parent_commands: &'a mut Vec<&'a crate::Command<U, E>>,
) -> Result<crate::ApplicationContext<'a, U, E>, crate::FrameworkError<'a, U, E>> {
    let ctx = extract_command(
        framework,
        global_commands,
        guild_commands,
        interaction,
        interaction_type,
        has_sent_initial_response,
        invocation_data,
        options,
        parent_commands,
    )?;
    super::common::check_permissions_and_cooldown(ctx.into()).await?;
    Ok(ctx)
}

/// Given the extracted application command data from [`extract_command`], runs the command,
/// including all the before and after code like checks.
async fn run_command<U: Send + Sync + 'static, E>(
    ctx: crate::ApplicationContext<'_, U, E>,
) -> Result<(), crate::FrameworkError<'_, U, E>> {
    super::common::check_permissions_and_cooldown(ctx.into()).await?;

    (ctx.framework.options.pre_command)(crate::Context::Application(ctx)).await;

    // Check which interaction type we received and grab the command action and, if context menu,
    // the resolved click target, and execute the action
    let command_structure_mismatch_error = crate::FrameworkError::CommandStructureMismatch {
        ctx,
        description: "received interaction type but command contained no \
                matching action or interaction contained no matching context menu object",
    };
    let action_result = match ctx.interaction.data.kind {
        serenity::CommandType::ChatInput => {
            let action = ctx
                .command
                .slash_action
                .ok_or(command_structure_mismatch_error)?;
            action(ctx).await
        }
        serenity::CommandType::User => {
            match (
                ctx.command.context_menu_action,
                &ctx.interaction.data.target(),
            ) {
                (
                    Some(crate::ContextMenuCommandAction::User(action)),
                    Some(serenity::ResolvedTarget::User(user, _)),
                ) => action(ctx, (*user).clone()).await,
                _ => return Err(command_structure_mismatch_error),
            }
        }
        serenity::CommandType::Message => {
            match (
                ctx.command.context_menu_action,
                &ctx.interaction.data.target(),
            ) {
                (
                    Some(crate::ContextMenuCommandAction::Message(action)),
                    Some(serenity::ResolvedTarget::Message(message)),
                ) => action(ctx, (*message).clone()).await,
                _ => return Err(command_structure_mismatch_error),
            }
        }
        other => {
            tracing::warn!("unknown interaction command type: {:?}", other);
            return Ok(());
        }
    };
    action_result?;

    (ctx.framework.options.post_command)(crate::Context::Application(ctx)).await;

    Ok(())
}

/// Dispatches this interaction onto framework commands, i.e. runs the associated command
pub async fn dispatch_interaction<'a, U: Send + Sync + 'static, E>(
    framework: crate::FrameworkContext<'a, U, E>,
    global_commands: &'a Guard<Arc<CommandStorage<U, E>>>,
    guild_commands: Option<(&'a Guard<Arc<CommandStorage<U, E>>>, &'a Guard<Arc<HashMap<String, CommandOverride>>>)>,
    interaction: &'a serenity::CommandInteraction,
    // Need to pass this in from outside because of lifetime issues
    has_sent_initial_response: &'a std::sync::atomic::AtomicBool,
    // Need to pass this in from outside because of lifetime issues
    invocation_data: &'a tokio::sync::Mutex<Box<dyn std::any::Any + Send + Sync>>,
    // Need to pass this in from outside because of lifetime issues
    options: &'a [serenity::ResolvedOption<'a>],
    parent_commands: &'a mut Vec<&'a crate::Command<U, E>>,
) -> Result<(), crate::FrameworkError<'a, U, E>> {
    let ctx = extract_command(
        framework,
        global_commands,
        guild_commands,
        interaction,
        crate::CommandInteractionType::Command,
        has_sent_initial_response,
        invocation_data,
        options,
        parent_commands,
    )?;

    crate::catch_unwind_maybe(run_command(ctx))
        .await
        .map_err(|payload| crate::FrameworkError::CommandPanic {
            payload,
            ctx: ctx.into(),
        })??;

    Ok(())
}

/// Given the extracted application command data from [`extract_command`], runs the autocomplete
/// callbacks, including all the before and after code like checks.
async fn run_autocomplete<U: Send + Sync + 'static, E>(
    ctx: crate::ApplicationContext<'_, U, E>,
) -> Result<(), crate::FrameworkError<'_, U, E>> {
    super::common::check_permissions_and_cooldown(ctx.into()).await?;

    // Find which parameter is focused by the user
    let (focused_option_name, partial_input) = match ctx.args.iter().find_map(|o| match &o.value {
        serenity::ResolvedValue::Autocomplete { value, .. } => Some((&o.name, value)),
        _ => None,
    }) {
        Some(x) => x,
        None => {
            tracing::warn!("no option is focused in autocomplete interaction");
            return Ok(());
        }
    };

    // Find the matching parameter from our Command object
    let parameters = &ctx.command.parameters;
    let focused_parameter = parameters
        .iter()
        .find(|p| &p.name == focused_option_name)
        .ok_or(crate::FrameworkError::CommandStructureMismatch {
            ctx,
            description: "focused autocomplete parameter name not recognized",
        })?;

    // Only continue if this parameter supports autocomplete and Discord has given us a partial value
    let autocomplete_callback = match focused_parameter.autocomplete_callback {
        Some(a) => a,
        _ => return Ok(()),
    };

    // Generate an autocomplete response
    let autocomplete_response = autocomplete_callback(ctx, partial_input).await;

    // Send the generates autocomplete response
    if let Err(e) = ctx
        .interaction
        .create_response(
            ctx.http(),
            serenity::CreateInteractionResponse::Autocomplete(autocomplete_response),
        )
        .await
    {
        tracing::warn!("couldn't send autocomplete response: {e}");
    }

    Ok(())
}

/// Dispatches this interaction onto framework commands, i.e. runs the associated autocomplete
/// callback
pub async fn dispatch_autocomplete<'a, U: Send + Sync + 'static, E>(
    framework: crate::FrameworkContext<'a, U, E>,
    global_commands: &'a Guard<Arc<CommandStorage<U, E>>>,
    guild_commands: Option<(&'a Guard<Arc<CommandStorage<U, E>>>, &'a Guard<Arc<HashMap<String, CommandOverride>>>)>,
    interaction: &'a serenity::CommandInteraction,
    // Need to pass the following in from outside because of lifetime issues
    has_sent_initial_response: &'a std::sync::atomic::AtomicBool,
    invocation_data: &'a tokio::sync::Mutex<Box<dyn std::any::Any + Send + Sync>>,
    options: &'a [serenity::ResolvedOption<'a>],
    parent_commands: &'a mut Vec<&'a crate::Command<U, E>>,
) -> Result<(), crate::FrameworkError<'a, U, E>> {
    let ctx = extract_command(
        framework,
        global_commands,
        guild_commands,
        interaction,
        crate::CommandInteractionType::Autocomplete,
        has_sent_initial_response,
        invocation_data,
        options,
        parent_commands,
    )?;

    crate::catch_unwind_maybe(run_autocomplete(ctx))
        .await
        .map_err(|payload| crate::FrameworkError::CommandPanic {
            payload,
            ctx: ctx.into(),
        })??;

    Ok(())
}
