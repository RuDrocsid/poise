//! The Command struct, which stores all information about a single framework command

use std::borrow::Cow;
use std::collections::HashMap;
use crate::structs::command_storage::{CommandStorage, CommandStorageConsistencyError};
use crate::{serenity_prelude as serenity, BoxFuture, CommandParameter, CommandParameterChoice};
use crate::{CowStr, CowVec};
use regex::Regex;
use std::iter::once;
use std::ops::Deref;
use std::sync::{Arc, LazyLock};
use tokio::sync::RwLock;

// Regex used to check command and option names
// https://discord.com/developers/docs/interactions/application-commands
static COMMAND_NAME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-_\p{L}\p{N}\p{sc=Deva}\p{sc=Thai}]{1,32}$").unwrap());

/// Default name given to commands
const DEFAULT_NAME: CowStr = Cow::Borrowed("A slash command");

// https://discord.com/developers/docs/reference#locales
const ALLOWED_LOCALES: [&str; 32] = [
    "id", "da", "de", "en-GB", "en-US", "es-ES", "es-419", "fr", "hr", "it", "lt", "hu", "nl",
    "no", "pl", "pt-BR", "ro", "fi", "sv-SE", "vi", "tr", "cs", "el", "bg", "ru", "uk", "hi", "th",
    "zh-CN", "ja", "zh-TW", "ko",
];

/// This struct allows to temporarily disable commands, their group (usage is up to the user) and
/// add or remove aliases.
///
/// This struct does not store any reference to a command.
/// It is supposed to be used as a value in a map.
#[derive(Debug, Clone)]
pub struct CommandOverride {
    /// Disables this command.
    /// The command will be ignored when poise determines the command to be executed.
    pub disabled: bool,
    /// Disables the group of this command.
    /// This can be used in any way the user wants and is just another way to disable a command
    /// without overwriting the disabled flag.
    /// E.g. if multiple commands are grouped together by a bot, this can be used to deactivate them,
    /// while not changing the disabled flag itself.
    pub group_disabled: bool,
    /// This field allows users to add (true) or remove (false) aliases from a command.
    pub changed_aliases: HashMap<String, bool>,
}

impl Default for CommandOverride {
    fn default() -> Self {
        Self {
            disabled: false,
            group_disabled: false,
            changed_aliases: HashMap::new(),
        }
    }
}

/// Possible errors found when checking command option choices for consistency.
///
/// These enum variants should always be wrapped in [`CommandOptionError::CommandOptionChoiceError`]
/// to provide more context.
pub enum CommandOptionChoiceError<'a> {
    /// `choice` has non-compliant `names`.
    NameNotCompliant {
        /// The choice which is affected
        choice: &'a CommandParameterChoice,
        /// Which choice names of this choice are non-compliant
        names: Vec<String>,
    },
}

/// Possible errors when checking each command option (parameter) for consistency.
///
/// These enum variants should always be wrapped in [`CommandConsistencyError::CommandOptionError`]
/// to provide more context.
pub enum CommandOptionError<'a, U, E> {
    /// Parameter `param` has non-compliant `names`.
    NameNotCompliant {
        /// Which param is affected
        param: &'a CommandParameter<U, E>,
        /// Which parameter names of this parameter are non-compliant
        names: Vec<String>,
    },
    /// Parameter `param` has non-compliant `descriptions`.
    DescriptionNotCompliant {
        /// Which param is affected
        param: &'a CommandParameter<U, E>,
        /// Which parameter descriptions of this parameter are non-compliant
        descriptions: Vec<String>,
    },
    /// Parameter `param` has too many choices.
    TooManyChoices {
        /// Which param is affected
        param: &'a CommandParameter<U, E>
    },
    /// Parameter `param` has non-compliant choices.
    CommandOptionChoiceError {
        /// Which param is affected
        param: &'a CommandParameter<U, E>,
        /// List of choice errors
        errs: Vec<CommandOptionChoiceError<'a>>,
    },
}

/// Possible errors produced by [`Command::check_consistency`].
pub enum CommandConsistencyError<'a, U, E> {
    /// This prefix or slash command is a subcommand,
    /// but is missing a parent command of the same type (prefix or slash).
    ParentCommandTypeWrong(&'a Command<U, E>),
    /// This slash command is a subcommand, which has three (or more) "levels" of parents above it.
    HierarchyLevelExceeded(&'a Command<U, E>),
    /// This command has more characters than allowed by Discord.
    MaxLengthExceeded(&'a Command<U, E>),
    /// This command is a context menu command with options, this is not allowed.
    ContextMenuWithOptions(&'a Command<U, E>),
    /// This `cmd` has non-compliant `names`.
    NameNotCompliant {
        /// Which command is affected
        cmd: &'a Command<U, E>,
        /// Which command names are non-compliant
        names: Vec<String>,
    },
    /// `cmd` has non-compliant `descriptions`.
    DescriptionNotCompliant {
        /// Which command is affected
        cmd: &'a Command<U, E>,
        /// Which command descriptions are non-compliant
        descriptions: Vec<String>,
    },
    /// The command has too many options / parameters
    /// (not too many subcommands, see [`CommandStorageConsistencyError::TooManySlashCommands`]).
    TooManyOptions {
        /// Which command is affected
        cmd: &'a Command<U, E>
    },
    /// `cmd` has two different `params` (options),
    /// which share one or multiple `names` (or localised names).
    ///
    /// These `params` are actually parameters, never subcommands. For subcommands sharing names
    /// see [`CommandStorageConsistencyError::DuplicatedCommandIdentifier`].
    OptionNameDuplicated {
        /// Which command is affected
        cmd: &'a Command<U, E>,
        /// Which two params have conflicting option names
        params: (&'a CommandParameter<U, E>, &'a CommandParameter<U, E>),
        /// Which names are in conflict
        names: Vec<String>,
    },
    /// One or multiple options of `cmd` failed their own consistency check.
    CommandOptionError {
        /// Which command is affected
        cmd: &'a Command<U, E>,
        /// List of option errors
        errs: Vec<CommandOptionError<'a, U, E>>,
    },
    /// The command storage for subcommands of `cmd` failed its [consistency check](CommandStorage::check_simple_consistency).
    SubcommandStorageConsistencyErrors {
        /// Which command is affected
        cmd: &'a Command<U, E>,
        /// List of errors for this subcommand storage
        errors: Vec<CommandStorageConsistencyError<'a, U, E>>,
    },
}

/// Type returned from `#[poise::command]` annotated functions, which contains all the generated
/// prefix and application commands
#[derive(derivative::Derivative)]
#[derivative(Default(bound = ""), Clone(bound = ""), Debug(bound = ""))]
pub struct Command<U, E> {
    // =============
    /// Callback to execute when this command is invoked in a prefix context
    #[derivative(Debug = "ignore")]
    pub prefix_action: Option<
        for<'a> fn(
            crate::PrefixContext<'a, U, E>,
        ) -> BoxFuture<'a, Result<(), crate::FrameworkError<'a, U, E>>>,
    >,
    /// Callback to execute when this command is invoked in a slash context
    #[derivative(Debug = "ignore")]
    pub slash_action: Option<
        for<'a> fn(
            crate::ApplicationContext<'a, U, E>,
        ) -> BoxFuture<'a, Result<(), crate::FrameworkError<'a, U, E>>>,
    >,
    /// Callback to execute when this command is invoked in a context menu context
    ///
    /// The enum variant shows which Discord item this context menu command works on
    pub context_menu_action: Option<crate::ContextMenuCommandAction<U, E>>,

    // ============= Command type agnostic data
    /// Subcommands of this command, if any
    pub subcommands: CommandStorage<U, E>,
    /// Require a subcommand to be invoked
    pub subcommand_required: bool,
    /// Main name of the command. Aliases (prefix-only) can be set in [`Self::aliases`].
    pub name: CowStr,
    /// Localized names with locale string as the key (slash-only)
    pub name_localizations: CowVec<(CowStr, CowStr)>,
    /// Full name including parent command names.
    ///
    /// Initially set to just [`Self::name`] and properly populated when the framework is started.
    pub qualified_name: CowStr,
    /// A string to identify this particular command within a list of commands.
    ///
    /// Can be configured via the [`crate::command`] macro (though it's probably not needed for most
    /// bots). If not explicitly configured, it falls back to the command function name.
    pub identifying_name: CowStr,
    /// The name of the `#[poise::command]`-annotated function
    pub source_code_name: CowStr,
    /// Identifier for the category that this command will be displayed in for help commands.
    pub category: Option<CowStr>,
    /// Whether to hide this command in help menus.
    pub hide_in_help: bool,
    /// Short description of the command. Displayed inline in help menus and similar.
    pub description: Option<CowStr>,
    /// Localized descriptions with locale string as the key (slash-only)
    pub description_localizations: CowVec<(CowStr, CowStr)>,
    /// Multiline description with detailed usage instructions. Displayed in the command specific
    /// help: `~help command_name`
    pub help_text: Option<CowStr>,
    /// if `true`, disables automatic cooldown handling before this commands invocation.
    ///
    /// Will override [`crate::FrameworkOptions::manual_cooldowns`] allowing manual cooldowns
    /// on select commands.
    pub manual_cooldowns: Option<bool>,
    /// Handles command cooldowns. Mainly for framework internal use
    pub cooldowns: Arc<tokio::sync::Mutex<crate::CooldownTracker>>,
    /// Configuration for the [`crate::CooldownTracker`]
    pub cooldown_config: Arc<RwLock<crate::CooldownConfig>>,
    /// After the first response, whether to post subsequent responses as edits to the initial
    /// message
    ///
    /// Note: in prefix commands, this only has an effect if
    /// `crate::PrefixFrameworkOptions::edit_tracker` is set.
    pub reuse_response: bool,
    /// Permissions which users must have to invoke this command. Used by Discord to set who can
    /// invoke this as a slash command. Not used on prefix commands or checked internally.
    ///
    /// Set to [`serenity::Permissions::empty()`] by default
    pub default_member_permissions: serenity::Permissions,
    /// Permissions which users must have to invoke this command.
    ///
    /// This is checked internally and works for both prefix commands and slash commands.
    ///
    /// This also handles the case a message is sent in a thread, in which `SEND_MESSAGES` is set to `SEND_MESSAGES_IN_THREADS`.
    ///
    /// Set to [`serenity::Permissions::empty()`] by default
    pub required_permissions: serenity::Permissions,
    /// Permissions without which command execution will fail.
    ///
    /// You can set this to fail early and give a descriptive error message in case the
    /// bot hasn't been assigned the minimum permissions by the guild admin.
    ///
    /// This also handles the case a message is sent in a thread, in which `SEND_MESSAGES` is set to `SEND_MESSAGES_IN_THREADS`.
    ///
    /// Set to [`serenity::Permissions::empty()`] by default
    pub required_bot_permissions: serenity::Permissions,
    /// If true, only users from the [owners list](crate::FrameworkOptions::owners) may use this
    /// command.
    pub owners_only: bool,
    /// If true, only people in guilds may use this command
    pub guild_only: bool,
    /// If true, the command may only run in DMs
    pub dm_only: bool,
    /// If true, the command may only run in NSFW channels
    pub nsfw_only: bool,
    /// Command-specific override for [`crate::FrameworkOptions::on_error`]
    #[derivative(Debug = "ignore")]
    pub on_error: Option<fn(crate::FrameworkError<'_, U, E>) -> BoxFuture<'_, ()>>,
    /// If any of these functions returns false, this command will not be executed.
    #[derivative(Debug = "ignore")]
    pub checks: Vec<fn(crate::Context<'_, U, E>) -> BoxFuture<'_, Result<bool, E>>>,
    /// List of parameters for this command
    ///
    /// Used for registering and parsing slash commands. Can also be used in help commands
    pub parameters: Vec<CommandParameter<U, E>>,
    /// Arbitrary data, useful for storing custom metadata about your commands
    #[derivative(Default(value = "Arc::new(())"))]
    pub custom_data: Arc<dyn std::any::Any + Send + Sync>,
    /// A unique ID for this command; can be used to identify this command in config files, databases, ...
    pub command_id: Option<String>,

    // ============= Prefix-specific data
    /// Alternative triggers for the command (prefix-only)
    pub aliases: CowVec<CowStr>,
    /// Whether to rerun the command if an existing invocation message is edited (prefix-only)
    pub invoke_on_edit: bool,
    /// Whether to delete the bot response if an existing invocation message is deleted (prefix-only)
    pub track_deletion: bool,
    /// Whether to broadcast a typing indicator while executing this command (prefix-only)
    pub broadcast_typing: bool,

    // ============= Application-specific data
    /// Context menu specific name for this command, displayed in Discord's context menu
    pub context_menu_name: Option<CowStr>,
    /// Whether responses to this command should be ephemeral by default (application-only)
    pub ephemeral: bool,
    /// List of installation contexts for this command (application-only)
    pub install_context: Option<Vec<serenity::InstallationContext>>,
    /// List of interaction contexts for this command (application-only)
    pub interaction_context: Option<Vec<serenity::InteractionContext>>,

    // Like #[non_exhaustive], but #[poise::command] still needs to be able to create an instance
    #[doc(hidden)]
    pub __non_exhaustive: (),
}

impl<U, E> PartialEq for Command<U, E> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl<U, E> Eq for Command<U, E> {}

/*
Tested below in the "tests" module are (verified on 30.1.25):
- Discord rejects command updates, if the commands contain locales, which are not recognized by Discord

Things ensured somewhere else when registering application commands:
- Discord does not allow existence of descriptions for context menu commands
- Options of application commands with subcommands are ignored
- Poise removes locales from application commands, if they are not recognized by Discord
 */
impl<U, E> Command<U, E> {
    /// Serializes this Command into an application command option, which is the form which Discord
    /// requires subcommands to be in
    fn create_as_subcommand(&self) -> Option<serenity::CreateCommandOption<'static>> {
        self.slash_action?;

        let kind = if self.subcommands.is_empty() {
            serenity::CommandOptionType::SubCommand
        } else {
            serenity::CommandOptionType::SubCommandGroup
        };

        let description = self.description.clone().unwrap_or(DEFAULT_NAME);
        let mut builder = serenity::CreateCommandOption::new(kind, self.name.clone(), description);

        for (locale, name) in self.name_localizations.iter() {
            if ALLOWED_LOCALES.contains(&&**locale) {
                builder = builder.name_localized(locale.clone(), name.clone());
            }
        }
        for (locale, description) in self.description_localizations.iter() {
            if ALLOWED_LOCALES.contains(&&**locale) {
                builder = builder.description_localized(locale.clone(), description.clone());
            }
        }

        if self.subcommands.is_empty() {
            for param in &self.parameters {
                // Using `?` because if this command has slash-incompatible parameters, we cannot
                // just ignore them but have to abort the creation process entirely
                builder = builder.add_sub_option(param.create_as_slash_command_option()?);
            }
        } else {
            for subcommand in self.subcommands.iter() {
                if let Some(subcommand) = subcommand.create_as_subcommand() {
                    builder = builder.add_sub_option(subcommand);
                }
            }
        }

        Some(builder)
    }

    /// Generates a slash command builder from this [`Command`] instance. This can be used
    /// to register this command on Discord's servers
    pub fn create_as_slash_command(&self) -> Option<serenity::CreateCommand<'static>> {
        self.slash_action?;

        let mut builder = serenity::CreateCommand::new(self.name.clone())
            .description(self.description.clone().unwrap_or(DEFAULT_NAME));

        for (locale, name) in self.name_localizations.iter() {
            if ALLOWED_LOCALES.contains(&&**locale) {
                builder = builder.name_localized(locale.clone(), name.clone());
            }
        }
        for (locale, description) in self.description_localizations.iter() {
            if ALLOWED_LOCALES.contains(&&**locale) {
                builder = builder.description_localized(locale.clone(), description.clone());
            }
        }

        // This is_empty check is needed because Discord special cases empty
        // default_member_permissions to mean "admin-only" (yes it's stupid)
        if !self.default_member_permissions.is_empty() {
            builder = builder.default_member_permissions(self.default_member_permissions);
        }

        if self.guild_only {
            builder = builder.contexts(vec![serenity::InteractionContext::Guild]);
        } else if self.dm_only {
            builder = builder.contexts(vec![serenity::InteractionContext::BotDm]);
        }

        if let Some(install_context) = self.install_context.clone() {
            builder = builder.integration_types(install_context);
        }

        if let Some(interaction_context) = self.interaction_context.clone() {
            builder = builder.contexts(interaction_context);
        }

        if self.subcommands.is_empty() {
            for param in &self.parameters {
                // Using `?` because if this command has slash-incompatible parameters, we cannot
                // just ignore them but have to abort the creation process entirely
                builder = builder.add_option(param.create_as_slash_command_option()?);
            }
        } else {
            for subcommand in self.subcommands.iter() {
                if let Some(subcommand) = subcommand.create_as_subcommand() {
                    builder = builder.add_option(subcommand);
                }
            }
        }

        Some(builder)
    }

    /// Generates a context menu command builder from this [`Command`] instance. This can be used
    /// to register this command on Discord's servers
    pub fn create_as_context_menu_command(&self) -> Option<serenity::CreateCommand<'static>> {
        let context_menu_action = self.context_menu_action?;

        // TODO: localization?
        let name = self.context_menu_name.clone().unwrap_or(self.name.clone());
        let mut builder = serenity::CreateCommand::new(name).kind(match context_menu_action {
            crate::ContextMenuCommandAction::User(_) => serenity::CommandType::User,
            crate::ContextMenuCommandAction::Message(_) => serenity::CommandType::Message,
            crate::ContextMenuCommandAction::__NonExhaustive => unreachable!(),
        });

        if self.guild_only {
            builder = builder.contexts(vec![serenity::InteractionContext::Guild]);
        } else if self.dm_only {
            builder = builder.contexts(vec![serenity::InteractionContext::BotDm]);
        }

        if let Some(install_context) = self.install_context.clone() {
            builder = builder.integration_types(install_context);
        }

        if let Some(interaction_context) = self.interaction_context.clone() {
            builder = builder.contexts(interaction_context);
        }

        Some(builder)
    }

    /// Updates the qualified name for this command and all subcommands of this command, to include
    /// the qualified name of the parent command.
    pub fn set_qualified_names_recursively(&mut self, parent_name: &Option<&CowStr>) {
        if let Some(parent_name) = parent_name {
            self.qualified_name = CowStr::Owned(format!("{} {}", parent_name, self.name));
        }

        self.subcommands
            .set_qualified_names(Some(&self.qualified_name));
    }

    /// Returns all collisions between `self` and `other` for
    ///
    /// * name
    /// * name_localizations
    /// * aliases
    ///
    /// if the commands share a command type (prefix, slash, contextmenu).
    ///
    /// Application command names should always be lower case, this is not ensured by this check.
    /// They will be rejected by Discord on registering as illegal otherwise.
    /// Non-lowercase application command names may yield false negatives in this check!
    ///
    /// `prefix_names_case_insensitive` if true, the names (and aliases)
    /// of prefix commands are checked case-insensitive.
    pub fn check_naming_collision(
        &self,
        other: &Self,
        prefix_names_case_insensitive: bool,
        overrides: &HashMap<String, CommandOverride>,
    ) -> Vec<String> {
        let default_override = CommandOverride::default();

        let command_override = self
            .command_id
            .as_ref()
            .and_then(|id| overrides.get(id))
            .unwrap_or(&default_override);
        if command_override.disabled || command_override.group_disabled {
            return vec![];
        }

        let other_command_override = other
            .command_id
            .as_ref()
            .and_then(|id| overrides.get(id))
            .unwrap_or(&default_override);
        if other_command_override.disabled || other_command_override.group_disabled {
            return vec![];
        }

        let context_check =
            self.context_menu_action.is_some() &&
                other.context_menu_action.is_some();
        let prefix_check = self.prefix_action.is_some() && other.prefix_action.is_some();
        let slash_check = self.slash_action.is_some() && other.slash_action.is_some();

        // create string comparing closure, which respects `prefix_names_case_insensitive`
        let string_equal = if prefix_names_case_insensitive {
            |first: &str, second: &str| first.eq_ignore_ascii_case(&*second)
        } else {
            |first: &str, second: &str| first == second
        };

        let mut conflicts: Vec<String> = vec![];

        // All checks are symmetrical, we do not need to check other way around.
        // context menu name: check the context_menu_name or normal name;
        // normal name is used for registering by poise, if context_menu_name does not exist
        if context_check && string_equal(
            &self.context_menu_name.clone().unwrap_or_else(|| self.name.clone()),
            &other.context_menu_name.clone().unwrap_or_else(|| other.name.clone())
        ) {
            conflicts.push(self.context_menu_name.clone().unwrap().into());
        }

        // name
        if slash_check || prefix_check {
            if string_equal(&self.name, &other.name) {
                conflicts.push(self.name.to_string());
            }

            // all localisations
            conflicts.extend(
                self.name_localizations
                    .iter()
                    .filter(|(key, value)| {
                        other
                            .name_localizations
                            .iter()
                            .any(|(key2, value2)| key == key2 && string_equal(value, value2))
                    })
                    .map(|(_, s)| s.deref().to_owned()),
            );
        }

        // all aliases
        if prefix_check {
            conflicts.extend(
                self
                    .aliases
                    .iter()
                    .filter(|a| command_override.changed_aliases.get(&a.to_string()) != Some(&false))
                    .map(|a| a.deref())
                    .chain(
                        command_override
                            .changed_aliases
                            .iter()
                            .filter_map(|(a, b)| if *b { Some(a.deref()) } else { None }),
                    )
                    .filter(|a| {
                        other
                            .aliases
                            .iter()
                            .filter(|a| {
                                other_command_override.changed_aliases.get(&a.to_string()) != Some(&false)
                            })
                            .map(|a| a.deref())
                            .chain(
                                other_command_override
                                    .changed_aliases
                                    .iter()
                                    .filter_map(|(a, b)| if *b { Some(a.deref()) } else { None }),
                            )
                            .any(|x| string_equal(x, a))
                    })
                    .map(|s| s.to_owned()),
            );
        }

        conflicts
    }

    /// This method checks for any inconsistencies of this command:
    ///
    /// * if this is a top level application command or a context menu command,
    ///   max total length is 8000 chars, which includes:
    ///   * everything, which has a length (i.e. is a String; names, descriptions, options, ...)
    ///   * for localised Strings, only the longest localisation (or the default if longest) counts
    ///   * for slash commands: all subcommands recursively, non slash subcommands are ignored
    /// * if this is a prefix / slash subcommand: this command must have a parent command of the same type (or no parent)
    /// * if this is a slash subcommand: the "hierarchy level" of this command can not be greater than 3
    /// * if this is a context menu command: this command can not have any options
    /// * application command: (localised) name: see below
    /// * slash command: (localised) description: 1-100 chars
    /// * check the command options for slash commands:
    ///   * if command has subcommands: max 25 subcommands
    ///   * if command no has subcommands: see [`Command::check_options`]
    /// * check all subcommands for inconsistencies (recursion)
    ///
    /// Rules for (localised) names of application commands and options:
    /// > `CHAT_INPUT` command names and command option names
    /// > must match the following regex
    /// > `^[-_\p{L}\p{N}\p{sc=Deva}\p{sc=Thai}]{1,32}$`
    /// > with the Unicode flag set. If there is a lowercase variant of any letters used, you must use those.
    /// > Characters with no lowercase variants and/or uncased letters are still allowed.
    /// > `USER` and `MESSAGE` commands may be mixed case and can include spaces.
    ///
    /// Prefix and slash commands (with subcommands) can have options. This is allowed,
    /// because poise can handle prefix commands with parameters under certain conditions.
    /// If this command is also a slash command and has subcommands, the options are ignored
    /// when creating the slash command.
    /// A prefix command with subcommands can be run as prefix command (even if it also exists as slash command).
    /// A slash only command with subcommands can not be run itself.
    ///
    /// The rules listed above are either required by poise or by Discord, listed here:
    /// https://discord.com/developers/docs/interactions/application-commands
    ///
    /// `command_level` is the level, which this command is located in the "command hierarchy".
    /// Top level commands have a value of 1.
    ///
    /// `prefix_names_case_insensitive` if true, the names (and aliases)
    /// of prefix commands are checked case-insensitive.
    pub fn check_consistency(
        &self,
        command_level: u8,
        prefix_names_case_insensitive: bool,
        parent_command: Option<&Self>,
        overrides: &HashMap<String, CommandOverride>,
    ) -> Vec<CommandConsistencyError<'_, U, E>> {
        let mut errors = vec![];

        // If this command is an application command and has no parent (i.e. a top level command),
        // calculate and check length
        if (self.slash_action.is_some() && command_level == 1 || self.context_menu_action.is_some())
            && self.calculate_command_length(self.slash_action.is_some()) > 8000
        {
            errors.push(CommandConsistencyError::MaxLengthExceeded(self));
        }

        // Check type of parent command if exists
        if let Some(parent) = parent_command {
            if self.prefix_action.is_some() && parent.prefix_action.is_none()
                || self.slash_action.is_some() && parent.slash_action.is_none()
            {
                errors.push(CommandConsistencyError::ParentCommandTypeWrong(self))
            }
        }

        // Check command hierarchy level if slash command
        if self.slash_action.is_some() && command_level > 3 {
            errors.push(CommandConsistencyError::HierarchyLevelExceeded(self));
        }

        // Check if context menu command has options
        if self.context_menu_action.is_some() && !self.parameters.is_empty() {
            errors.push(CommandConsistencyError::ContextMenuWithOptions(self));
        }

        // check all (localised) names
        // ignore prefix only commands
        if self.slash_action.is_some() || self.context_menu_action.is_some() {
            let mut names = self
                .name_localizations
                .iter()
                .map(|(_, s)| s)
                .chain(once(&self.name))
                .filter(|s| !Command::<U, E>::check_name(s, false))
                .map(|s| s.deref().to_owned())
                .collect::<Vec<_>>();

            if let Some(name) = &self.context_menu_name {
                if !Self::check_name(name, true) {
                    names.push(name.deref().to_owned());
                }
            }

            if !names.is_empty() {
                errors.push(CommandConsistencyError::NameNotCompliant { cmd: self, names });
            }
        }

        // Check length of all (localised) descriptions.
        // If this command is not a slash command, ignore it.
        // Descriptions of context menu commands are ignored by Discord anyway, no need to check.
        if self.slash_action.is_some() {
            let mut descriptions = self
                .description_localizations
                .iter()
                .map(|(_, s)| s)
                .filter(|s| s.is_empty() || s.len() > 100)
                .map(|s| s.deref().to_owned())
                .collect::<Vec<_>>();

            if let Some(description) = &self.description {
                if description.is_empty() || description.len() > 100 {
                    descriptions.push(description.deref().to_owned());
                }
            }

            if !descriptions.is_empty() {
                errors.push(CommandConsistencyError::DescriptionNotCompliant {
                    cmd: self,
                    descriptions,
                });
            }
        }

        // Check options if slash command and has no subcommands
        if self.slash_action.is_some() && self.subcommands.is_empty() {
            errors.extend(self.check_options());
        }

        // Recurse, increase level by 1,
        // because our subcommands are one level deeper than this command
        let subcommand_errors = self.subcommands.check_simple_consistency(
            command_level + 1,
            prefix_names_case_insensitive,
            Some(self),
            overrides
        );

        if !subcommand_errors.is_empty() {
            errors.push(
                CommandConsistencyError::SubcommandStorageConsistencyErrors {
                    cmd: self,
                    errors: subcommand_errors,
                },
            );
        }

        errors
    }

    /// This method checks if any option (parameter) of this command violates Discord's rules:
    ///
    /// * max amount: 25
    /// * (localised) name: same as command name (see [`Command::check_consistency`])
    /// * (localised) description: 1-100 chars
    /// * a (localised) option name can not exist in another option's
    ///    * localisations for the same locale
    ///    * non-localised name
    /// * choices:
    ///    * max amount: 25
    ///    * (localised) name: 1-100 chars
    ///    * value: max 100 chars                               TODO does not exist in poise??
    ///
    /// Using this method only makes sense, if this command is a slash command without subcommands!
    fn check_options(&self) -> Vec<CommandConsistencyError<'_, U, E>> {
        let mut option_errors = vec![];
        let mut command_errors = vec![];

        // Do not check commands with subcommands
        if !self.subcommands.is_empty() {
            return vec![];
        }

        // max 25
        if self.parameters.len() > 25 {
            command_errors.push(CommandConsistencyError::TooManyOptions { cmd: self })
        }

        // Check all options (in this case parameters)
        // Check all (localised) names
        for parameter in &self.parameters {
            let names = parameter
                .name_localizations
                .iter()
                .map(|(_, l)| l)
                .chain(once(&parameter.name))
                .filter(|l| !Command::<U, E>::check_name(l, false))
                .map(|s| s.deref().to_owned())
                .collect::<Vec<_>>();

            if !names.is_empty() {
                option_errors.push(CommandOptionError::NameNotCompliant {
                    param: parameter,
                    names,
                });
            }
        }

        // check descriptions
        for parameter in &self.parameters {
            let mut descriptions = parameter
                .description_localizations
                .iter()
                .map(|(_, l)| l)
                .filter(|l| l.is_empty() || l.len() > 100)
                .map(|s| s.deref().to_owned())
                .collect::<Vec<_>>();

            if let Some(description) = &self.description {
                if description.is_empty() || description.len() > 100 {
                    descriptions.push(description.deref().to_owned());
                }
            }

            if !descriptions.is_empty() {
                option_errors.push(CommandOptionError::DescriptionNotCompliant {
                    param: parameter,
                    descriptions,
                });
            }
        }

        // Check for any naming conflicts in option names
        /* We need to check, that no (localised) name of "param"
         is present as (localised) name of "other_param".

         We first collect all (localised) names for "param"
         and compare each of them to all (localised) names of "other_param".

         We use vecs, so the iterators must not be reevaluated every time.
        */
        // Iterate over all params
        let mut iter = self.parameters.iter();
        while let Some(param) = iter.next() {
            let param_names = param
                .name_localizations
                .iter()
                .map(|(_, l)| l)
                .chain(once(&param.name))
                .collect::<Vec<_>>();

            // iterate over all params after "param"
            let mut iter2 = iter.clone().skip(1);
            while let Some(other_param) = iter2.next() {
                let other_names = other_param
                    .name_localizations
                    .iter()
                    .map(|(_, l)| l)
                    .chain(once(&other_param.name))
                    .collect::<Vec<_>>();

                let conflicts = param_names
                    .iter()
                    .filter(|name| other_names.iter().any(|other_name| other_name.eq(*name)))
                    .map(|s| (*s).deref().to_owned())
                    .collect::<Vec<_>>();

                if !conflicts.is_empty() {
                    command_errors.push(CommandConsistencyError::OptionNameDuplicated {
                        cmd: self,
                        params: (param, other_param),
                        names: conflicts,
                    });
                }
            }
        }

        // Check all parameters for choices and their consistency
        for parameter in &self.parameters {
            // not more than 25 choices per parameter
            if parameter.choices.len() > 25 {
                option_errors.push(CommandOptionError::TooManyChoices { param: parameter });
            }

            let mut choice_errors = vec![];

            // Check choice names for each parameter
            for choice in parameter.choices.iter() {
                let names = choice
                    .localizations
                    .iter()
                    .map(|(_, s)| s)
                    .chain(once(&choice.name))
                    .filter(|n| n.is_empty() || n.len() > 100)
                    .map(|s| s.deref().to_owned())
                    .collect::<Vec<_>>();

                if !names.is_empty() {
                    choice_errors.push(CommandOptionChoiceError::NameNotCompliant {
                        choice: choice,
                        names,
                    })
                }
            }

            if !choice_errors.is_empty() {
                option_errors.push(CommandOptionError::CommandOptionChoiceError {
                    param: parameter,
                    errs: choice_errors,
                });
            }
        }

        if !option_errors.is_empty() {
            command_errors.push(CommandConsistencyError::CommandOptionError {
                cmd: self,
                errs: option_errors,
            })
        }

        command_errors
    }

    /// Returns the "string length" of this command, which is calculated:
    ///
    /// * as sum of everything, which has a length (i.e. is a String; names, descriptions, options, ...)
    /// * recursively with all subcommands, non slash subcommands are ignored
    /// * for localised Strings, only the longest localisation (or the default if longest) counts
    ///
    /// `slash_recurse`:
    ///   - If false:
    ///     - Recursion is disabled.
    ///     - If no context menu action is defined, this command will be ignored.
    ///   - If true and a slash action is defined, the length of this command will be calculated
    ///     and recursion is active.
    fn calculate_command_length(&self, slash_recurse: bool) -> u16 {
        if (!slash_recurse && self.context_menu_action.is_none()) ||
            (slash_recurse && self.slash_action.is_none()) {
            return 0;
        }

        let mut length: u16 = 0;

        // Add up all parameters if slash command and no subcommands.
        // Slash command with subcommands or context menu commands have no parameters in Discord.
        if self.slash_action.is_some() && self.subcommands.is_empty() {
            length += self
                .parameters
                .iter()
                .map(|p| {
                    // add up all choices if existing
                    p.choices.iter().map(|c|
                        // take the longest name
                        c.localizations
                            .iter()
                            .map(|(_, d)| d.len())
                            .chain(once(c.name.len()))
                            .max()
                            .unwrap_or(0) as u16
                    ).sum::<u16>() +

                    // add the biggest name length
                    p.name_localizations
                        .iter()
                        .map(|(_, d)| d.len())
                        .chain(once(p.name.len()))
                        .max()
                        .unwrap_or(0) as u16 +

                    // add the biggest description length
                    p.description_localizations
                        .iter()
                        .map(|(_, d)| d.len())
                        .chain(once(p.description.clone().map(|s| s.len()).unwrap_or(0)))
                        .max()
                        .unwrap_or(0) as u16
                })
                .sum::<u16>();
        }

        // the longest name of this command
        length += self
            .name_localizations
            .iter()
            .map(|(_, d)| d.len())
            .chain(once(self.name.len()))
            .max()
            .unwrap_or(0) as u16;

        // the longest description of this command
        length += self
            .description_localizations
            .iter()
            .map(|(_, d)| d.len())
            .chain(once(self.description.clone().map(|s| s.len()).unwrap_or(0)))
            .max()
            .unwrap_or(0) as u16;

        // recurse
        if slash_recurse {
            for command in self.subcommands.iter() {
                length += Command::calculate_command_length(command, true);
            }
        }

        length
    }

    /// Checks if `name` matches `COMMAND_NAME_REGEX`.
    ///
    /// If `allow_mixed_case` is false, the name must be lowercase (slash command names)
    /// else the casing is ignored (context menu command names).
    ///
    /// This is used for command names and option names.
    fn check_name(name: &str, allow_mixed_case: bool) -> bool {
        COMMAND_NAME_REGEX.is_match(name) && (allow_mixed_case || name.to_lowercase().eq(name))
    }
}

// these tests check some invariants, which are used by the command consistency check functions
#[allow(unused)]
mod test {
    use super::*;
    use crate as poise;
    use ::serenity::all::{Http, GuildId, HttpError};
    use crate::builtins::create_application_commands;

    const TEST_GUILD_ID: u64 = 0; // CHANGEME

    struct Data {} // User data, which is stored and accessible in all command invocations
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Context<'a> = crate::Context<'a, Data, Error>;

    // the commands were generated using poises macro, because the generated code contains "::poise::",
    // we can not use it directly without fixing it to "poise::"

    fn test_slash_command() -> poise::Command<
        <Context<'static> as poise::_GetGenerics>::U,
        <Context<'static> as poise::_GetGenerics>::E,
    > {
        use ::std::borrow::Cow;
        async fn inner(_: Context<'_>) -> Result<(), Error> {
            Ok(())
        }
        poise::Command {
            prefix_action: Some(|ctx| {
                Box::pin(async move {
                    let ( ..   ) = poise::parse_prefix_args!(ctx . serenity_context (), ctx . msg , ctx . args , 0 => ).await.map_err(|(error, input)| poise::FrameworkError::new_argument_parse(ctx.into(), input, error))?;
                    let is_framework_cooldown = !ctx
                        .command
                        .manual_cooldowns
                        .unwrap_or_else(|| ctx.framework.options.manual_cooldowns);
                    if is_framework_cooldown {
                        ctx.command
                            .cooldowns
                            .lock()
                            .await
                            .start_cooldown(ctx.cooldown_context());
                    }
                    inner(ctx.into())
                        .await
                        .map_err(|error| poise::FrameworkError::new_command(ctx.into(), error))
                })
            }),
            slash_action: Some(|ctx| {
                Box::pin(async move {
                    let () = poise::parse_slash_args!(ctx . serenity_context (), ctx . interaction , ctx . args => ).await.map_err(|error| error.to_framework_error(ctx))?;
                    let is_framework_cooldown = !ctx
                        .command
                        .manual_cooldowns
                        .unwrap_or_else(|| ctx.framework.options.manual_cooldowns);
                    if is_framework_cooldown {
                        ctx.command
                            .cooldowns
                            .lock()
                            .await
                            .start_cooldown(ctx.cooldown_context());
                    }
                    inner(ctx.into())
                        .await
                        .map_err(|error| poise::FrameworkError::new_command(ctx.into(), error))
                })
            }),
            context_menu_action: None,
            subcommands: vec![].into(),
            subcommand_required: false,
            name: Cow::Borrowed("test_command"),
            name_localizations: Cow::Borrowed(&[]),
            qualified_name: Cow::Borrowed("test_command"),
            identifying_name: Cow::Borrowed("test_command"),
            source_code_name: Cow::Borrowed("test_command"),
            category: None,
            description: None,
            description_localizations: Cow::Borrowed(&[]),
            help_text: None,
            hide_in_help: false,
            manual_cooldowns: None,
            cooldowns: std::sync::Arc::new(tokio::sync::Mutex::new(poise::Cooldowns::new())),
            cooldown_config: ::std::sync::Arc::new(::tokio::sync::RwLock::default()),
            reuse_response: false,
            default_member_permissions: poise::serenity_prelude::Permissions::empty(),
            required_permissions: poise::serenity_prelude::Permissions::empty(),
            required_bot_permissions: poise::serenity_prelude::Permissions::empty(),
            owners_only: false,
            guild_only: false,
            dm_only: false,
            nsfw_only: false,
            install_context: None,
            interaction_context: None,
            checks: vec![],
            on_error: None,
            parameters: vec![],
            custom_data: ::std::sync::Arc::new(()),
            command_id: None,
            aliases: Cow::Borrowed(&[]),
            invoke_on_edit: false,
            track_deletion: false,
            broadcast_typing: false,
            context_menu_name: None,
            ephemeral: false,
            __non_exhaustive: (),
        }
    }

    // parse token and stuff, return http object
    async fn get_http() -> Http {
        let token = serenity::Token::from_env("DISCORD_TOKEN").unwrap();
        let http = serenity::HttpBuilder::new(token)
            .ratelimiter_disabled(true)
            .build();

        let info = http
            .get_current_application_info()
            .await
            .expect("missing application info");
        http.set_application_id(info.id);
        http
    }

    // Test: Discord rejects commands with non-allowed locales
    #[tokio::test]
    #[ignore] // can not be run automatically, because it needs a token
    async fn test_wrong_locales() {
        let mut command = test_slash_command();
        command
            .name_localizations
            .to_mut()
            .push((CowStr::Borrowed("eng-test"), CowStr::Borrowed("eng-test")));

        let http = get_http().await;
        let guild_id: GuildId = TEST_GUILD_ID.into();

        let storage = CommandStorage::from(vec![command]);
        let mut builder = create_application_commands(&storage);
        let mut command = builder.pop().unwrap();
        command = command.name_localized("eng-test", "eng-test");
        builder.push(command);
        let res = guild_id.set_commands(&http, &*builder).await;
        assert!(res.is_err());

        let error = res.unwrap_err();
        let serenity::Error::Http(HttpError::UnsuccessfulRequest(response)) = error else {
            assert!(false);
            return;
        };

        assert_eq!(response.status_code, 400);
        let json = response.error;
        assert_eq!(json.code.0, 50035);
    }
}
