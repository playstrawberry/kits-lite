//! Editable inventory kits, persistent cooldowns and read-only previews.
#![forbid(unsafe_code)]
mod commands;
mod component_kinds;
mod host;
mod item_data;
mod kits;
mod lang;
mod model;
mod settings;
mod store;
use kits::{Kits, say_error};
use std::{path::PathBuf, sync::Arc};
use strawberry_plugin_api::{
    Context, EventHandler, Plugin, PluginMetadata, Result, Server, events,
    events::{FailureVerdict, HandlerOptions, HandlerPriority},
    events_wit::{
        InventoryClickEventData, InventoryCloseEventData, InventoryDragEventData,
        PlayerJoinEventData, PlayerLeaveEventData,
    },
    permissions, register_plugin,
};
struct Click(Arc<Kits>);
impl EventHandler<events::InventoryClickEvent> for Click {
    fn handle(
        &self,
        _server: Server,
        mut event: InventoryClickEventData,
    ) -> InventoryClickEventData {
        if !self.0.has_menu(&event.player) {
            return event;
        }
        let already_cancelled = event.cancelled;
        event.cancelled = true;
        if !already_cancelled
            && event.window_type == Some(strawberry_plugin_api::Screen::Generic9x6)
            && let Err(error) =
                self.0
                    .click(&event.player, event.slot, event.raw_slot, event.click_type)
        {
            say_error(&event.player, &error, "");
        }
        event
    }
}
struct Drag(Arc<Kits>);
impl EventHandler<events::InventoryDragEvent> for Drag {
    fn handle(&self, _server: Server, mut event: InventoryDragEventData) -> InventoryDragEventData {
        if self.0.has_menu(&event.player) {
            event.cancelled = true;
        }
        event
    }
}
struct Close(Arc<Kits>);
impl EventHandler<events::InventoryCloseEvent> for Close {
    fn handle(&self, _server: Server, event: InventoryCloseEventData) -> InventoryCloseEventData {
        self.0.close_menu(&event.player);
        event
    }
}
struct Leave(Arc<Kits>);
impl EventHandler<events::PlayerLeaveEvent> for Leave {
    fn handle(&self, _server: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        self.0.leave(&event.player);
        event
    }
}
struct Join(Arc<Kits>);
impl EventHandler<events::PlayerJoinEvent> for Join {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        if !event.cancelled
            && let Err(error) = self.0.first_join(&event.player)
        {
            say_error(&event.player, &error, "");
        }
        event
    }
}
struct KitsLite {
    kits: Arc<Kits>,
}
impl Plugin for KitsLite {
    fn new() -> Self {
        Self {
            kits: Arc::new(Kits::default()),
        }
    }
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "kits-lite".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Strawberry Interactive LLC".into()],
            description: lang::text("en", "kit_command", &[]),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                permissions::FS_WRITE_DATA.into(),
            ],
        }
    }
    fn on_load(&self, context: Context) -> Result<()> {
        let dir = PathBuf::from(context.get_data_folder());
        std::fs::create_dir_all(&dir).map_err(|_| lang::text("en", "load_error", &[]))?;
        let settings = settings::load(&dir).map_err(|problem| problem.message())?;
        for definition in &settings.definitions {
            for (key, _) in &definition.items {
                if !host::known_item(key) {
                    let text = key.clone();
                    return Err(settings::Problem { line: 0, text }.message());
                }
            }
        }
        lang::configure(settings.fixed_language.clone(), settings.texts.clone());
        let settings = Arc::new(settings);
        self.kits
            .load(dir, Arc::clone(&settings))
            .map_err(|_| lang::text("en", "load_error", &[]))?;
        for permission in commands::permissions(&self.kits) {
            context.register_permission(&permission)?;
        }
        commands::register(&context, &self.kits, &settings);
        let protection = HandlerOptions {
            priority: HandlerPriority::Highest,
            blocking: true,
            ignore_cancelled: false,
            on_failure: FailureVerdict::Deny,
        };
        let cleanup = HandlerOptions {
            priority: HandlerPriority::Normal,
            blocking: true,
            ignore_cancelled: false,
            on_failure: FailureVerdict::Allow,
        };
        let join = HandlerOptions {
            priority: HandlerPriority::Highest,
            blocking: true,
            ignore_cancelled: true,
            on_failure: FailureVerdict::Allow,
        };
        // A feature that is off costs nothing: its handlers are never registered.
        if settings.has_menus() {
            context.register_event_handler_with_options::<events::InventoryClickEvent, _>(
                Click(Arc::clone(&self.kits)),
                protection,
            )?;
            context.register_event_handler_with_options::<events::InventoryDragEvent, _>(
                Drag(Arc::clone(&self.kits)),
                protection,
            )?;
            context.register_event_handler_with_options::<events::InventoryCloseEvent, _>(
                Close(Arc::clone(&self.kits)),
                cleanup,
            )?;
            context.register_event_handler_with_options::<events::PlayerLeaveEvent, _>(
                Leave(Arc::clone(&self.kits)),
                cleanup,
            )?;
        }
        if settings.first_join {
            context.register_event_handler_with_options::<events::PlayerJoinEvent, _>(
                Join(Arc::clone(&self.kits)),
                join,
            )?;
        }
        self.kits.set_context(context);
        Ok(())
    }
    fn on_unload(&self, _context: Context) -> Result<()> {
        self.kits.stop();
        Ok(())
    }
}
register_plugin!(KitsLite);
