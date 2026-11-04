//! Commands route every delivery through the same checked claim boundary.
use crate::{
    host,
    kits::{Change, Kits, allowed, colored, permission},
    lang,
    model::{Error, MAX_COOLDOWN, player_id, valid_name},
    settings::{ListMode, Settings},
};
use std::sync::Arc;
use strawberry_plugin_api::{
    Context, Player, Server,
    command::{
        Arg, ArgumentType, Command, CommandError, CommandNode, CommandSender, ConsumedArgs,
        StringType,
    },
    commands::CommandHandler,
    common::NamedColor,
    logging::{LogLevel, log},
    permission::{Permission, PermissionDefault, PermissionLevel},
};
pub const PERM_KIT: &str = "kits-lite:kit";
pub const PERM_KITS: &str = "kits-lite:kits";
pub const PERM_ADMIN: &str = "kits-lite:admin";
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    List,
    Claim,
    Preview,
    Menu,
    Admin,
}
struct Run {
    kits: Arc<Kits>,
    settings: Arc<Settings>,
    action: Action,
}
fn value(args: &ConsumedArgs, key: &str) -> String {
    match args.get_value(key) {
        Arg::Simple(text) | Arg::Msg(text) => text,
        _ => String::new(),
    }
}
fn player(sender: &CommandSender) -> Result<Player, Error> {
    sender.as_player().ok_or(Error::PlayerOnly)
}
impl CommandHandler for Run {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let locale = sender
            .as_player()
            .map_or_else(|| "en".to_string(), |player| player.get_locale());
        // Case never matters; messages show the name as it was stored.
        let typed = value(&args, "name");
        let name = self
            .kits
            .catalog()
            .kit(&typed)
            .map_or(typed, |kit| kit.name.clone());
        let outcome = self.run(&sender, &server, &args, &locale, &name);
        outcome.map(|()| 1).map_err(|error| {
            if self.action == Action::Claim
                && error == Error::Unknown
                && !self.settings.plugin_unknown_reply
            {
                return CommandError::CommandFailed(
                    strawberry_plugin_api::text::TextComponent::translate(
                        &lang::text(&locale, "unknown_command", &[]),
                        Vec::new(),
                    ),
                );
            }
            CommandError::CommandFailed(colored(
                &lang::error(&locale, &error, &name),
                NamedColor::Red,
            ))
        })
    }
}
impl Run {
    fn run(
        &self,
        sender: &CommandSender,
        server: &Server,
        args: &ConsumedArgs,
        locale: &str,
        name: &str,
    ) -> Result<(), Error> {
        match self.action {
            Action::List => {
                let player = player(sender)?;
                let catalog = self.kits.catalog();
                let record = self
                    .kits
                    .player_record(player_id(&player.get_id().to_string())?)?;
                let parts: Vec<_> = catalog
                    .kits
                    .iter()
                    .filter(|kit| allowed(&player, kit))
                    .map(|kit| {
                        lang::text(
                            locale,
                            "entry",
                            &[&kit.name, &self.kits.status(locale, &record, kit)],
                        )
                    })
                    .collect();
                let text = if parts.is_empty() {
                    lang::text(locale, "list_empty", &[])
                } else {
                    lang::text(locale, "list", &[&parts.join(", ")])
                };
                sender.send_message(colored(&text, NamedColor::Gold));
            }
            Action::Claim => {
                let player = player(sender)?;
                self.kits.claim(&player, name)?;
                crate::kits::say(&player, "claimed", &[name]);
            }
            Action::Preview => self.kits.preview(&player(sender)?, name)?,
            Action::Menu => self.kits.open_menu(&player(sender)?, 0)?,
            Action::Admin => {
                if !sender.has_permission(server, PERM_ADMIN) {
                    return Err(Error::Permission);
                }
                let options = value(args, "options");
                if options.len() > 256 {
                    return Err(Error::Invalid);
                }
                let fields: Vec<_> = options.split_ascii_whitespace().take(7).collect();
                let action = *fields.first().ok_or(Error::Invalid)?;
                let typed = *fields.get(1).ok_or(Error::Invalid)?;
                let stored = self
                    .kits
                    .catalog()
                    .kit(typed)
                    .map_or_else(|_| typed.to_string(), |kit| kit.name.clone());
                let name = if action == "create" { typed } else { &stored };
                let change = match action {
                    "create" if (2..=4).contains(&fields.len()) => {
                        if !valid_name(name) {
                            return Err(Error::Invalid);
                        }
                        if self.kits.catalog().kit(name).is_ok() {
                            return Err(Error::Exists);
                        }
                        let cooldown = fields
                            .get(2)
                            .unwrap_or(&"600")
                            .parse::<u64>()
                            .map_err(|_| Error::Invalid)?;
                        if !(1..=MAX_COOLDOWN).contains(&cooldown) {
                            return Err(Error::Invalid);
                        }
                        let once = match fields.get(3).copied() {
                            None | Some("false") => false,
                            Some("true") => true,
                            _ => return Err(Error::Invalid),
                        };
                        Change::Create {
                            name: name.into(),
                            cooldown,
                            once,
                            entries: host::capture(&player(sender)?)?,
                        }
                    }
                    "edit" if fields.len() == 2 => {
                        self.kits.catalog().kit(name)?;
                        Change::Edit {
                            name: name.into(),
                            entries: host::capture(&player(sender)?)?,
                        }
                    }
                    "delete" if fields.len() == 2 => Change::Delete(name.into()),
                    "give" if fields.len() == 3 => {
                        let target = fields[2];
                        if target.is_empty()
                            || target.len() > 16
                            || !target
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                        {
                            return Err(Error::Invalid);
                        }
                        let target = server.get_player_by_name(target).ok_or(Error::Offline)?;
                        self.kits.claim(&target, name)?;
                        let actor = sender
                            .as_player()
                            .map_or_else(|| lang::text("en", "console", &[]), |p| p.get_name());
                        log(
                            LogLevel::Info,
                            &lang::text("en", "give_log", &[&actor, name, &target.get_name()]),
                        );
                        sender.send_message(colored(
                            &lang::text(locale, "given", &[name, &target.get_name()]),
                            NamedColor::Gold,
                        ));
                        return Ok(());
                    }
                    "set" if fields.len() == 4 => match fields[2] {
                        "cooldown" => Change::Cooldown(
                            name.into(),
                            fields[3].parse().map_err(|_| Error::Invalid)?,
                        ),
                        "once" => Change::Once(
                            name.into(),
                            match fields[3] {
                                "true" => true,
                                "false" => false,
                                _ => return Err(Error::Invalid),
                            },
                        ),
                        _ => return Err(Error::Invalid),
                    },
                    "firstjoin" if !self.settings.first_join => return Err(Error::Disabled),
                    "firstjoin" if fields.len() == 2 => {
                        Change::FirstJoin((name != "off").then(|| name.to_string()))
                    }
                    "resetuses" if fields.len() == 3 && fields[2] == "confirm" => {
                        self.kits.catalog().kit(name)?;
                        Change::ResetUses(Some(name.into()))
                    }
                    "resetalluses" if fields.len() == 2 && name == "confirm" => {
                        Change::ResetUses(None)
                    }
                    "importuses" if fields.len() == 2 && name == "confirm" => Change::ImportUses,
                    _ => return Err(Error::Invalid),
                };
                let actor = sender
                    .as_player()
                    .map(|player| Ok((player.get_name(), player_id(&player.get_id().to_string())?)))
                    .transpose()?;
                self.kits.change(server, actor, change, name.into())?;
            }
        }
        Ok(())
    }
}
fn node(name: &str, description: &str, default: PermissionDefault) -> Permission {
    Permission {
        node: name.into(),
        description: description.into(),
        default,
        children: Vec::new(),
    }
}
pub fn permissions(kits: &Kits) -> Vec<Permission> {
    let mut out = vec![
        node(
            PERM_KIT,
            &lang::text("en", "use_permission", &[]),
            PermissionDefault::Allow,
        ),
        node(
            PERM_KITS,
            &lang::text("en", "menu_permission", &[]),
            PermissionDefault::Allow,
        ),
        node(
            PERM_ADMIN,
            &lang::text("en", "admin_permission", &[]),
            PermissionDefault::Op(PermissionLevel::Four),
        ),
    ];
    for kit in &kits.catalog().kits {
        out.push(kit_permission(kit));
    }
    out
}
pub fn kit_permission(kit: &crate::model::Kit) -> Permission {
    node(
        &permission(&kit.name),
        &lang::text("en", "kit_permission", &[&kit.name]),
        if kit.restricted {
            PermissionDefault::Op(PermissionLevel::Four)
        } else {
            PermissionDefault::Allow
        },
    )
}
fn labels(first: &str, extra: &[String]) -> Vec<String> {
    let mut out = vec![first.to_string()];
    out.extend_from_slice(extra);
    out
}
fn list_action(mode: ListMode) -> Action {
    match mode {
        ListMode::Text => Action::List,
        ListMode::Menu => Action::Menu,
    }
}
pub fn register(context: &Context, kits: &Arc<Kits>, settings: &Arc<Settings>) {
    let run = |action| Run {
        kits: Arc::clone(kits),
        settings: Arc::clone(settings),
        action,
    };
    let word = ArgumentType::String(StringType::SingleWord);
    let mut kit = Command::new(
        &labels("kit", &settings.kit_labels),
        &lang::text("en", "kit_command", &[]),
    )
    .execute(run(list_action(settings.kit_list)))
    .then(CommandNode::argument("name", &word).execute(run(Action::Claim)));
    if settings.preview {
        kit = kit.then(
            CommandNode::literal("preview")
                .then(CommandNode::argument("name", &word).execute(run(Action::Preview))),
        );
    }
    context.register_command(kit, PERM_KIT);
    context.register_command(
        Command::new(
            &labels("kits", &settings.kits_labels),
            &lang::text("en", "kits_command", &[]),
        )
        .execute(run(list_action(settings.kits_list))),
        PERM_KITS,
    );
    context.register_command(
        Command::new(
            &["kitadmin".into()],
            &lang::text("en", "admin_command", &[]),
        )
        .then(
            CommandNode::argument("options", &ArgumentType::String(StringType::Greedy))
                .execute(run(Action::Admin)),
        ),
        PERM_ADMIN,
    );
}
