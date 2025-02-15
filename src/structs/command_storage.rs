use crate::structs::CowStr;
use crate::{Command, CommandConsistencyError, CommandOverride, ContextMenuCommandAction};
use arc_swap::{ArcSwap, Guard};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};

/// These are errors, which can be returned by [`CommandStorage::check_top_level_consistency`]
/// or [`CommandStorage::check_simple_consistency`].
pub enum CommandStorageConsistencyError<'a, U, E> {
    /// The checked command storage contains more slash commands than allowed.
    ///
    /// The current limit is 100 for top level commands and 25 for the subcommands of a command.
    TooManySlashCommands(u16),
    /// The checked command storage contains more context menu commands (of any type) than allowed.
    /// This includes all "levels", because context menu commands can not be subcommands,
    /// and poise searches for them recursively.
    ///
    /// The found commands are given as a Vec.
    TooManyContextCommands(Vec<&'a Command<U, E>>),
    /// Two commands in the checked storage have the same identifier, this is not allowed.
    ///
    /// This can be the default name, a localised name or an alias.
    DuplicatedCommandIdentifier {
        /// Which commands have conflicting identifiers
        cmds: (&'a Command<U, E>, &'a Command<U, E>),
        /// `identifiers` will contain all the duplicated identifiers for these two commands.
        identifiers: Vec<String>,
    },
    /// Contains commands of the checked storage, which failed their own [consistency check](Command::check_consistency).
    CommandConsistencyError(Vec<CommandConsistencyError<'a, U, E>>),
}

/// This struct stores an UpdateableCommandStorage in an Arc to make it cloneable.
pub type UpdateableCommandStorageArc<U, E> = UpdateableArcSwapArc<CommandStorage<U, E>>;

impl<U, E> From<Vec<Command<U, E>>> for UpdateableCommandStorageArc<U, E> {
    fn from(value: Vec<Command<U, E>>) -> Self {
        let command_storage: CommandStorage<U, E> = value.into();
        command_storage.into()
    }
}

/// This struct stores a CommandStorage in an UpdateableArcSwap to allow synchronized mutation.
pub type UpdateableCommandStorage<U, E> = UpdateableArcSwap<CommandStorage<U, E>>;

/// This struct stores an UpdateableArcSwap in an Arc to make it cloneable.
#[derive(Debug)]
pub struct UpdateableArcSwapArc<T>(pub Arc<UpdateableArcSwap<T>>);

impl<T> Deref for UpdateableArcSwapArc<T> {
    type Target = UpdateableArcSwap<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> From<UpdateableArcSwap<T>> for UpdateableArcSwapArc<T> {
    fn from(value: UpdateableArcSwap<T>) -> Self {
        Self(Arc::new(value.into()))
    }
}

impl<T> From<T> for UpdateableArcSwapArc<T> {
    fn from(value: T) -> Self {
        Self(Arc::new(value.into()))
    }
}

/// An ArcSwap, which holds some data.
///
/// To update the data you have two options:
/// - You can just clone the data, modify it and use the [`UpdateableArcSwap::store`] method.
///   This option does not prevent modifications between cloning and storing and may therefore
///   lead to "lost updates".
/// - Lock, clone, update:
///   This option will prevent "lost updates" if done correctly:
///   1. Use [`UpdateableArcSwap::get_write_lock`] to lock this storage.
///   2. Clone the data.
///   3. Make your changes.
///   4. Use [`UpdateableArcSwap::update`] to store the cloned data.
///      This method will take the lock by value and drop it.
#[derive(Debug)]
pub struct UpdateableArcSwap<T> {
    data: ArcSwap<T>,
    lock: Mutex<()>,
}

impl<T> From<T> for UpdateableArcSwap<T> {
    fn from(value: T) -> Self {
        Self {
            data: ArcSwap::new(Arc::new(value)),
            lock: Mutex::default(),
        }
    }
}

impl<T: Default> Default for UpdateableArcSwap<T> {
    fn default() -> Self {
        Self {
            data: ArcSwap::new(Arc::new(T::default())),
            lock: Mutex::default(),
        }
    }
}

impl<T: Clone> UpdateableArcSwap<T> {
    /// Get the lock for this storage. This is used to synchronise updates of this storage.
    pub async fn get_write_lock(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().await
    }

    /// Replaces the data in this storage, while requiring
    /// synchronisation to prevent "lost updates".
    pub async fn update(&self, lock: MutexGuard<'_, ()>, new_data: T) {
        self.data.store(Arc::new(new_data));
        drop(lock);
    }

    /// Replaces the data in this storage, while not requiring
    /// synchronisation to prevent "lost updates".
    pub async fn store(&self, new_data: T) {
        self.data.store(Arc::new(new_data));
    }

    /// Returns a guard to the stored data.
    pub fn deref_owned(&self) -> Guard<Arc<T>> {
        self.data.load()
    }

    /// Returns the ArcSwap, which guards the stored data.
    /// This allows updating of the stored data without requiring synchronisation.
    pub fn get_raw_arc(&self) -> &ArcSwap<T> {
        &self.data
    }
}

/// A Vec, which holds multiple [`Command`] objects.
#[derive(derivative::Derivative)]
#[derivative(Default(bound = ""), Clone(bound = ""), Debug(bound = ""))]
pub struct CommandStorage<U, E>(Vec<Command<U, E>>);

impl<U, E> Deref for CommandStorage<U, E> {
    type Target = Vec<Command<U, E>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<U, E> DerefMut for CommandStorage<U, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<U, E> From<Vec<Command<U, E>>> for CommandStorage<U, E> {
    fn from(value: Vec<Command<U, E>>) -> Self {
        Self(value)
    }
}

impl<U, E> CommandStorage<U, E> {
    /// Returns an empty CommandStorage.
    /// This function uses a Vec with a capacity of 0.
    pub fn new_empty() -> Self {
        Self(Vec::with_capacity(0).into())
    }

    /// Returns all collisions between all prefix commands on the top level of `self` and `other` for
    ///
    /// * name
    /// * name_localizations
    /// * aliases.
    ///
    /// `prefix_names_case_insensitive` if true, the names (and aliases)
    /// of prefix commands are checked case-insensitive.
    ///
    /// This method will return a Vec of [`CommandStorageConsistencyError::DuplicatedCommandIdentifier`].
    pub fn check_collision_with_other<'a>(
        &'a self,
        other: &'a Self,
        prefix_names_case_insensitive: bool,
        overrides: &HashMap<String, CommandOverride>,
    ) -> Vec<CommandStorageConsistencyError<'a, U, E>> {
        let mut errors = vec![];

        // Create string comparing closures, which respect `prefix_names_case_insensitive`
        let string_equal = if prefix_names_case_insensitive {
            |first: &str, second: &str| first.eq_ignore_ascii_case(&*second)
        } else {
            |first: &str, second: &str| first == second
        };

        let default_override = CommandOverride::default();

        for command in self.iter() {
            if command.prefix_action.is_none() {
                continue;
            }

            let command_override = command
                .command_id
                .as_ref()
                .and_then(|id| overrides.get(id))
                .unwrap_or(&default_override);
            if command_override.disabled || command_override.group_disabled {
                continue;
            }

            for other_command in other.iter() {
                if other_command.prefix_action.is_none() {
                    continue;
                }

                let other_command_override = other_command
                    .command_id
                    .as_ref()
                    .and_then(|id| overrides.get(id))
                    .unwrap_or(&default_override);
                if other_command_override.disabled || other_command_override.group_disabled {
                    continue;
                }

                let mut identifiers = vec![];
                // all localisations
                identifiers.extend(
                    command
                        .name_localizations
                        .iter()
                        .filter(|(key, value)| {
                            other_command
                                .name_localizations
                                .iter()
                                .any(|(key2, value2)| key == key2 && string_equal(value, value2))
                        })
                        .map(|(_, s)| s.deref().to_owned()),
                );

                identifiers.extend(
                    command
                        .aliases
                        .iter()
                        .filter(|a| {
                            command_override.changed_aliases.get(&a.to_string()) != Some(&false)
                        })
                        .map(|a| a.deref())
                        .chain(
                            command_override
                                .changed_aliases
                                .iter()
                                .filter_map(|(a, b)| if *b { Some(a.deref()) } else { None }),
                        )
                        .filter(|a| {
                            other_command
                                .aliases
                                .iter()
                                .filter(|a| {
                                    other_command_override.changed_aliases.get(&a.to_string())
                                        != Some(&false)
                                })
                                .map(|a| a.deref())
                                .chain(
                                    other_command_override.changed_aliases.iter().filter_map(
                                        |(a, b)| if *b { Some(a.deref()) } else { None },
                                    ),
                                )
                                .any(|x| string_equal(x, a))
                        })
                        .map(|s| s.to_owned()),
                );

                if !identifiers.is_empty() {
                    errors.push(
                        CommandStorageConsistencyError::DuplicatedCommandIdentifier {
                            cmds: (command, other_command),
                            identifiers,
                        },
                    );
                }
            }
        }

        errors
    }

    ///   This method checks, if the stored commands comply to Discord's and poise's rules.
    ///
    ///   Because the rules are different for top level commands and their subcommands,
    ///   this method should only be used to check the top level commands of a bot.
    ///   It will automatically check all subcommands recursively.
    ///
    ///   To fulfill Discord's and poise's rules, the following conditions must be true:
    ///
    ///   * max 100 top level slash commands
    ///   * max 5 context menu commands per type (message / user)
    ///   * no two context menu commands can have the same (localised) name (if they are of the same type)
    ///   * no two slash commands on the same "level" can have the same (localised) name
    ///   * no two prefix commands on the same "level" can have the same (localised) name or alias
    ///   * every command must pass its own  [consistency check](`Command::check_consistency`)
    ///
    ///   You can find the documentation for Discord's rules here:
    ///   https://discord.com/developers/docs/interactions/application-commands
    ///
    ///   Because rules for guild specific and global commands are the same,
    ///   this method can be used to check both variants.
    ///   However, this method can not differentiate between guild specific and global commands,
    ///   so the given CommandStorage must not contain global and guild specific commands at the same time.
    ///
    ///   This check should be called after localization is done
    ///   and  [`CommandStorage::set_qualified_names`]  was called!
    ///
    ///   if `prefix_names_case_insensitive` is true, the names (and aliases)
    ///   of prefix commands are checked case-insensitive.
    ///   Slash command names are always lower case.
    ///
    ///   If violations are found, they are returned as errors.
    pub fn check_top_level_consistency(
        &self,
        prefix_names_case_insensitive: bool,
        overrides: &HashMap<String, CommandOverride>,
    ) -> Vec<CommandStorageConsistencyError<'_, U, E>> {
        let mut errors = vec![];

        if self.iter().filter(|c| c.slash_action.is_some()).count() > 100 {
            errors.push(CommandStorageConsistencyError::TooManySlashCommands(100));
        }

        let (u, m) = self.get_context_commands();

        let mut iter = u.iter();
        while let Some(command) = iter.next() {
            while let Some(other_command) = iter.clone().skip(1).next() {
                let names = command.check_naming_collision(
                    other_command,
                    prefix_names_case_insensitive,
                    overrides,
                );
                if !names.is_empty() {
                    errors.push(
                        CommandStorageConsistencyError::DuplicatedCommandIdentifier {
                            cmds: (command, other_command),
                            identifiers: names,
                        },
                    )
                }
            }
        }

        if u.len() > 5 {
            errors.push(CommandStorageConsistencyError::TooManyContextCommands(u));
        }

        let mut iter = m.iter();
        while let Some(command) = iter.next() {
            while let Some(other_command) = iter.clone().skip(1).next() {
                let names = command.check_naming_collision(
                    other_command,
                    prefix_names_case_insensitive,
                    overrides,
                );
                if !names.is_empty() {
                    errors.push(
                        CommandStorageConsistencyError::DuplicatedCommandIdentifier {
                            cmds: (command, other_command),
                            identifiers: names,
                        },
                    )
                }
            }
        }

        if m.len() > 5 {
            errors.push(CommandStorageConsistencyError::TooManyContextCommands(m));
        }

        errors.extend(self.check_simple_consistency(
            1,
            prefix_names_case_insensitive,
            None,
            overrides,
        ));

        errors
    }

    ///   This method checks, if the stored (sub)commands comply to Discord's and poise's rules.
    ///
    ///   This will NOT check things, which only make sense for top level commands
    ///   (e.g. max 100 slash commands on the first level), see  [`CommandStorage::check_top_level_consistency`] .
    ///
    ///   To fulfill Discord's and poise's rules for (sub)commands, the following conditions must be true:
    ///
    ///   * no two slash commands on the same "level" can have the same (localised) name
    ///   * no two prefix commands on the same "level" can have the same (localised) name or alias
    ///   * not more than 25 slash commands in this storage, if the storage is not "level" 1
    ///   * every command must pass its own  [consistency check](`Command::check_consistency`)
    ///
    ///   You can find the documentation for Discord's rules here:
    ///   https://discord.com/developers/docs/interactions/application-commands
    ///
    ///   Because rules for guild specific and global commands are the same,
    ///   this method can be used to check both variants.
    ///   However, this method can not differentiate between guild specific and global commands,
    ///   so the given CommandStorage must not contain global and guild specific commands at the same time.
    ///
    ///   This check should be called after localization is done
    ///   and  [`CommandStorage::set_qualified_names`]  was called!
    ///
    ///   `storage_level` is the level in the command hierarchy, which this storage is located at.
    ///   Top level storage is 1.
    ///
    ///   if `prefix_names_case_insensitive` is true, the names (and aliases)
    ///   of prefix commands are checked case-insensitive.
    ///   Application command names are always lower case.
    ///
    ///   If violations are found, they are returned as errors.
    pub fn check_simple_consistency(
        &self,
        storage_level: u8,
        prefix_names_case_insensitive: bool,
        parent_command: Option<&Command<U, E>>,
        overrides: &HashMap<String, CommandOverride>,
    ) -> Vec<CommandStorageConsistencyError<'_, U, E>> {
        let mut errors = vec![];

        if storage_level > 1 {
            if self.iter().filter(|c| c.slash_action.is_some()).count() > 25 {
                errors.push(CommandStorageConsistencyError::TooManySlashCommands(25));
            }
        }

        let mut iter = self.iter();
        while let Some(command) = iter.next() {
            let consistency_errors = command.check_consistency(
                storage_level,
                prefix_names_case_insensitive,
                parent_command,
                overrides,
            );
            if !consistency_errors.is_empty() {
                errors.push(CommandStorageConsistencyError::CommandConsistencyError(
                    consistency_errors,
                ));
            }

            while let Some(other_command) = iter.clone().skip(1).next() {
                let names = command.check_naming_collision(
                    other_command,
                    prefix_names_case_insensitive,
                    overrides,
                );
                if !names.is_empty() {
                    errors.push(
                        CommandStorageConsistencyError::DuplicatedCommandIdentifier {
                            cmds: (command, other_command),
                            identifiers: names,
                        },
                    )
                }
            }
        }

        errors
    }

    ///   This fixes the qualified names for all commands and subcommands of this CommandStorage.
    pub fn set_qualified_names(&mut self, parent_name: Option<&CowStr>) {
        for command in self.iter_mut() {
            command.set_qualified_names_recursively(&parent_name);
        }
    }

    ///   Returns the context commands bound to (user, message) objects.
    ///
    ///   This method searches recursively.
    pub fn get_context_commands(&self) -> (Vec<&Command<U, E>>, Vec<&Command<U, E>>) {
        let mut user_commands: Vec<_> = self
            .iter()
            .filter(|c| {
                matches!(
                    c.context_menu_action,
                    Some(ContextMenuCommandAction::User(_))
                )
            })
            .collect();
        let mut message_commands: Vec<_> = self
            .iter()
            .filter(|c| {
                matches!(
                    c.context_menu_action,
                    Some(ContextMenuCommandAction::Message(_))
                )
            })
            .collect();

        for c in self.iter() {
            let (u1, m1) = c.subcommands.get_context_commands();
            user_commands.extend(u1);
            message_commands.extend(m1);
        }
        (user_commands, message_commands)
    }
}
