//! One catalog, on-demand persistence and bounded read-only menu sessions.

use crate::commands::{PERM_ADMIN, PERM_KIT, PERM_KITS};
use crate::{
    host, lang,
    model::{Catalog, Entry, Error, Given, MAX_COOLDOWN, PlayerId, PlayerRecord, player_id},
    settings::{self, Settings},
    store,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};
use strawberry_plugin_api::{
    Context, ItemStack, Player, Screen, Server,
    common::{ClickType, NamedColor},
    gui::Gui,
    logging::{LogLevel, log},
    scheduler::SchedulerExt,
    text::TextComponent,
};

const MAX_MENUS: usize = 1024;
const PAGE_SIZE: usize = 45;
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
pub fn colored(text: &str, color: NamedColor) -> TextComponent {
    TextComponent::text(text).color_named(color)
}
pub fn say(player: &Player, key: &str, args: &[&str]) {
    player.send_system_message(
        colored(
            &lang::text(&player.get_locale(), key, args),
            NamedColor::Gold,
        ),
        false,
    );
}
pub fn say_error(player: &Player, error: &Error, name: &str) {
    player.send_system_message(
        colored(
            &lang::error(&player.get_locale(), error, name),
            NamedColor::Red,
        ),
        false,
    );
}
pub fn permission(name: &str) -> String {
    format!("kits-lite:kit.{name}")
}
pub fn allowed(player: &Player, kit: &crate::model::Kit) -> bool {
    player.has_permission(PERM_KIT) && player.has_permission(&permission(&kit.name))
}

#[derive(Clone)]
enum Menu {
    List {
        page: usize,
        names: Vec<(u32, String)>,
    },
    Preview {
        name: String,
    },
}
struct State {
    catalog: Arc<Catalog>,
    context: Option<Arc<Context>>,
    settings: Arc<Settings>,
    registered: BTreeSet<String>,
    dir: Option<PathBuf>,
    busy: bool,
    active: bool,
    menus: BTreeMap<PlayerId, Menu>,
    players: BTreeMap<PlayerId, PlayerRecord>,
}
pub struct Kits(Mutex<State>);
impl Default for Kits {
    fn default() -> Self {
        Self(Mutex::new(State {
            catalog: Arc::new(Catalog::default()),
            context: None,
            settings: Arc::new(Settings::default()),
            registered: BTreeSet::new(),
            dir: None,
            busy: false,
            active: false,
            menus: BTreeMap::new(),
            players: BTreeMap::new(),
        }))
    }
}
#[derive(Clone)]
pub enum Change {
    Create {
        name: String,
        entries: Vec<Entry>,
        cooldown: u64,
        once: bool,
    },
    Edit {
        name: String,
        entries: Vec<Entry>,
    },
    Delete(String),
    Cooldown(String, u64),
    Once(String, bool),
    FirstJoin(Option<String>),
    /// Forgets the uses of one kit, or of every kit.
    ResetUses(Option<String>),
    ImportUses,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    Configured,
    UsesReset,
    Imported(usize),
}
fn done_text(locale: &str, done: Done, name: &str) -> String {
    match done {
        Done::Configured => lang::text(locale, "configured", &[name]),
        Done::UsesReset => lang::text(locale, "uses_reset", &[]),
        Done::Imported(count) => lang::text(locale, "imported", &[&count.to_string()]),
    }
}
fn editable(catalog: &Catalog, name: &str) -> Result<(), Error> {
    if catalog.kit(name)?.listed {
        return Err(Error::Defined);
    }
    Ok(())
}
impl Change {
    pub fn apply(self, catalog: &mut Catalog) -> Result<(), Error> {
        match self {
            Self::Create {
                name,
                entries,
                cooldown,
                once,
            } => {
                catalog.create(&name, entries, cooldown, once)?;
            }
            Self::Edit { name, entries } => {
                editable(catalog, &name)?;
                catalog.edit(&name, entries)?;
            }
            Self::Delete(name) => {
                editable(catalog, &name)?;
                catalog.delete(&name)?;
            }
            Self::Cooldown(name, value) => {
                if !(1..=MAX_COOLDOWN).contains(&value) {
                    return Err(Error::Invalid);
                }
                editable(catalog, &name)?;
                let id = catalog.kit(&name)?.id;
                catalog
                    .kits
                    .iter_mut()
                    .find(|kit| kit.id == id)
                    .ok_or(Error::Unknown)?
                    .cooldown = value;
            }
            Self::Once(name, value) => {
                editable(catalog, &name)?;
                let id = catalog.kit(&name)?.id;
                catalog
                    .kits
                    .iter_mut()
                    .find(|kit| kit.id == id)
                    .ok_or(Error::Unknown)?
                    .one_time = value;
            }
            Self::FirstJoin(name) => {
                catalog.first_join = match name {
                    Some(name) => Some(catalog.kit(&name)?.id),
                    None => None,
                };
            }
            Self::ResetUses(name) => catalog.renew_uses(name.as_deref())?,
            Self::ImportUses => return Err(Error::Invalid),
        }
        catalog.validate()
    }
}
impl Kits {
    pub fn load(&self, dir: PathBuf, settings: Arc<Settings>) -> Result<(), Error> {
        let mut catalog = store::initialize(&dir)?;
        let stored = catalog.clone();
        settings::apply(&settings, &mut catalog)?;
        if catalog != stored {
            store::save(&dir, &catalog)?;
        }
        let mut state = lock(&self.0);
        state.catalog = Arc::new(catalog);
        state.settings = settings;
        state.dir = Some(dir);
        state.active = true;
        Ok(())
    }
    pub fn stop(&self) {
        let mut state = lock(&self.0);
        state.active = false;
        state.menus.clear();
        state.players.clear();
        state.context = None;
        state.registered.clear();
    }
    pub fn set_context(&self, context: Context) {
        let mut state = lock(&self.0);
        state.registered = state
            .catalog
            .kits
            .iter()
            .map(|kit| kit.name.clone())
            .collect();
        state.context = Some(Arc::new(context));
    }
    pub fn catalog(&self) -> Arc<Catalog> {
        Arc::clone(&lock(&self.0).catalog)
    }
    pub fn claim(&self, player: &Player, name: &str) -> Result<(), Error> {
        self.begin_change()?;
        let result = self.claim_inner(player, name);
        lock(&self.0).busy = false;
        result
    }
    fn claim_inner(&self, player: &Player, name: &str) -> Result<(), Error> {
        let catalog = self.catalog();
        let kit = catalog.kit(name)?;
        if !allowed(player, kit) {
            return Err(Error::Permission);
        }
        let id = player_id(&player.get_id().to_string())?;
        let mut record = self.player_record(id)?;
        let now = now_secs();
        record.eligible(kit, now)?;
        let prepared = host::prepare(player, kit)?;
        let previous = record.clone();
        record.record(&catalog, kit, now)?;
        // A crash after this save may lose one grant; the saved use is never replayed.
        self.save_record(id, &record)?;
        match prepared.give(kit) {
            Given::All => Ok(()),
            // Nothing was written, so restoring the old record cannot duplicate anything.
            Given::Nothing => {
                self.save_record(id, &previous)?;
                Err(Error::Full)
            }
            Given::Partial(lost) => {
                log(
                    LogLevel::Warn,
                    &lang::text(
                        "en",
                        "partial_log",
                        &[&player.get_name(), &kit.name, &lost.to_string()],
                    ),
                );
                Err(Error::Partial)
            }
        }
    }
    pub fn player_record(&self, id: PlayerId) -> Result<PlayerRecord, Error> {
        let dir = {
            let state = lock(&self.0);
            if let Some(record) = state.players.get(&id) {
                return Ok(record.clone());
            }
            state.dir.clone().ok_or(Error::Storage)?
        };
        let record = store::load_player(&dir, id)?;
        remember(&mut lock(&self.0), id, record.clone());
        Ok(record)
    }
    fn save_record(&self, id: PlayerId, record: &PlayerRecord) -> Result<(), Error> {
        let dir = lock(&self.0).dir.clone().ok_or(Error::Storage)?;
        store::save_player(&dir, id, record)?;
        remember(&mut lock(&self.0), id, record.clone());
        Ok(())
    }
    pub fn status(&self, locale: &str, record: &PlayerRecord, kit: &crate::model::Kit) -> String {
        let eligible = record.eligible(kit, now_secs());
        match eligible {
            Err(Error::Cooldown(left)) => lang::text(locale, "wait", &[&left.to_string()]),
            Err(Error::Once) => lang::text(locale, "used", &[]),
            Err(error) => lang::error(locale, &error, &kit.name),
            Ok(()) => lang::text(locale, "ready", &[]),
        }
    }
    /// One on-demand async write; no repeating task, unbounded queue or lock across I/O.
    pub fn change(
        self: &Arc<Self>,
        server: &Server,
        actor: Option<(String, PlayerId)>,
        change: Change,
        name: String,
    ) -> Result<(), Error> {
        self.begin_change()?;
        let shared = Arc::clone(self);
        server.schedule_delayed_async_task(1, move |server| {
            let player = actor.as_ref().and_then(|(name, id)| {
                server
                    .get_player_by_name(name)
                    .filter(|player| player_id(&player.get_id().to_string()) == Ok(*id))
            });
            let result: Result<Done, Error> = if actor.is_some()
                && player
                    .as_ref()
                    .is_none_or(|player| !player.has_permission(PERM_ADMIN))
            {
                Err(Error::Permission)
            } else {
                shared.commit_change(change.clone())
            };
            lock(&shared.0).busy = false;
            if let Some(player) = player {
                let locale = player.get_locale();
                match result {
                    Ok(done) => player.send_system_message(
                        colored(&done_text(&locale, done, &name), NamedColor::Gold),
                        false,
                    ),
                    Err(error) => say_error(&player, &error, &name),
                }
            } else if actor.is_none() {
                let text = match result {
                    Ok(done) => done_text("en", done, &name),
                    Err(error) => lang::error("en", &error, &name),
                };
                log(LogLevel::Info, &text);
            }
        });
        Ok(())
    }
    fn begin_change(&self) -> Result<(), Error> {
        let mut state = lock(&self.0);
        if state.busy || !state.active {
            return Err(Error::Busy);
        }
        state.busy = true;
        Ok(())
    }
    fn commit_change(&self, change: Change) -> Result<Done, Error> {
        let (original, dir) = {
            let state = lock(&self.0);
            if !state.active {
                return Err(Error::Busy);
            }
            (
                Arc::clone(&state.catalog),
                state.dir.clone().ok_or(Error::Storage)?,
            )
        };
        if matches!(change, Change::ImportUses) {
            let changed = store::import_uses(&dir, &original, now_secs())?;
            lock(&self.0).players.clear();
            return Ok(Done::Imported(changed));
        }
        let done = if matches!(change, Change::ResetUses(_)) {
            Done::UsesReset
        } else {
            Done::Configured
        };
        let mut candidate = (*original).clone();
        change.apply(&mut candidate)?;
        let context = lock(&self.0).context.clone();
        for kit in candidate
            .kits
            .iter()
            .filter(|kit| original.kit(&kit.name).is_err())
        {
            if let Some(context) = &context {
                {
                    let state = lock(&self.0);
                    if state.registered.contains(&kit.name) {
                        continue;
                    }
                    if state.registered.len() >= crate::model::MAX_KITS {
                        return Err(Error::Limit);
                    }
                }
                context
                    .register_permission(&crate::commands::kit_permission(kit))
                    .map_err(|_| Error::Permission)?;
                lock(&self.0).registered.insert(kit.name.clone());
            }
        }
        store::save(&dir, &candidate)?;
        lock(&self.0).catalog = Arc::new(candidate);
        Ok(done)
    }
    fn settings(&self) -> Arc<Settings> {
        Arc::clone(&lock(&self.0).settings)
    }
    pub fn open_menu(&self, player: &Player, requested_page: usize) -> Result<(), Error> {
        if !player.has_permission(PERM_KITS) {
            return Err(Error::Permission);
        }
        let id = player_id(&player.get_id().to_string())?;
        {
            let state = lock(&self.0);
            if state.menus.len() >= MAX_MENUS && !state.menus.contains_key(&id) {
                return Err(Error::Limit);
            }
        }
        let catalog = self.catalog();
        let kits: Vec<_> = catalog
            .kits
            .iter()
            .filter(|kit| allowed(player, kit))
            .collect();
        let record = self.player_record(id)?;
        let locale = player.get_locale();
        let settings = self.settings();
        let hint = match (settings.menu_claim, settings.preview) {
            (true, true) => Some("hint"),
            (true, false) => Some("hint_claim"),
            (false, true) => Some("hint_preview"),
            (false, false) => None,
        };
        let page = requested_page.min(kits.len().saturating_sub(1) / PAGE_SIZE);
        let gui = menu_gui(&lang::text(&player.get_locale(), "menu", &[]));
        let mut names = Vec::new();
        for (index, kit) in kits
            .iter()
            .skip(page * PAGE_SIZE)
            .take(PAGE_SIZE)
            .enumerate()
        {
            let slot = list_slot(index);
            let icon = host::render(&kit.entries[0].stack)?;
            icon.set_custom_name(Some(TextComponent::text(&kit.name)));
            icon.add_lore(TextComponent::text(&self.status(&locale, &record, kit)));
            if let Some(hint) = hint {
                icon.add_lore(TextComponent::text(&lang::text(&locale, hint, &[])));
            }
            gui.set_item(slot, icon);
            names.push((slot, kit.name.clone()));
        }
        if page > 0 {
            button(&gui, 45, "minecraft:arrow", player, "previous");
        }
        if (page + 1) * PAGE_SIZE < kits.len() {
            button(&gui, 53, "minecraft:arrow", player, "next");
        }
        player.open_gui(gui);
        lock(&self.0).menus.insert(id, Menu::List { page, names });
        Ok(())
    }
    pub fn preview(&self, player: &Player, name: &str) -> Result<(), Error> {
        let catalog = self.catalog();
        let kit = catalog.kit(name)?;
        if !allowed(player, kit) {
            return Err(Error::Permission);
        }
        let id = player_id(&player.get_id().to_string())?;
        {
            let state = lock(&self.0);
            if state.menus.len() >= MAX_MENUS && !state.menus.contains_key(&id) {
                return Err(Error::Limit);
            }
        }
        let gui = menu_gui(&lang::text(&player.get_locale(), "preview", &[name]));
        for entry in &kit.entries {
            gui.set_item(u32::from(entry.slot), host::render(&entry.stack)?);
        }
        let settings = self.settings();
        if settings.has_list_menu() {
            button(&gui, 45, "minecraft:arrow", player, "back");
        }
        if settings.menu_claim {
            button(&gui, 49, "minecraft:lime_dye", player, "claim");
        }
        player.open_gui(gui);
        lock(&self.0)
            .menus
            .insert(id, Menu::Preview { name: name.into() });
        Ok(())
    }
    pub fn has_menu(&self, player: &Player) -> bool {
        if lock(&self.0).menus.is_empty() {
            return false;
        }
        player_id(&player.get_id().to_string())
            .is_ok_and(|id| lock(&self.0).menus.contains_key(&id))
    }
    pub fn close_menu(&self, player: &Player) {
        if lock(&self.0).menus.is_empty() {
            return;
        }
        if let Ok(id) = player_id(&player.get_id().to_string()) {
            lock(&self.0).menus.remove(&id);
        }
    }
    pub fn click(
        &self,
        player: &Player,
        slot: i16,
        raw_slot: i16,
        click: ClickType,
    ) -> Result<(), Error> {
        if !valid_click(slot, raw_slot, click) {
            return Ok(());
        }
        let menu = lock(&self.0)
            .menus
            .get(&player_id(&player.get_id().to_string())?)
            .cloned();
        let settings = self.settings();
        match menu {
            Some(Menu::List { page, names }) => {
                if slot == 45 && page > 0 {
                    return self.open_menu(player, page - 1);
                }
                if slot == 53 {
                    return self.open_menu(player, page + 1);
                }
                if let Some((_, name)) = names.iter().find(|(index, _)| *index == slot as u32) {
                    if click == ClickType::Right {
                        if settings.preview {
                            self.preview(player, name)?;
                        }
                    } else if settings.menu_claim {
                        self.claim_and_report(player, name);
                    }
                }
            }
            Some(Menu::Preview { name }) => {
                if slot == 45 && settings.has_list_menu() {
                    self.open_menu(player, 0)?;
                } else if slot == 49 && settings.menu_claim {
                    self.claim_and_report(player, &name);
                }
            }
            None => {}
        }
        Ok(())
    }
    fn claim_and_report(&self, player: &Player, name: &str) {
        match self.claim(player, name) {
            Ok(()) => say(player, "claimed", &[name]),
            Err(error) => say_error(player, &error, name),
        }
    }
    pub fn first_join(&self, player: &Player) -> Result<(), Error> {
        let id = player_id(&player.get_id().to_string())?;
        if self.player_record(id)?.seen {
            return Ok(());
        }
        let catalog = self.catalog();
        if let Some(kit) = catalog
            .first_join
            .and_then(|id| catalog.kits.iter().find(|kit| kit.id == id))
        {
            self.claim(player, &kit.name)?;
            say(player, "claimed", &[&kit.name]);
        } else if self.begin_change().is_ok() {
            // Under the gate, so a running use import cannot be overwritten. If the gate is taken
            // the player is simply recorded at a later join.
            let result = self.player_record(id).and_then(|mut record| {
                record.seen = true;
                self.save_record(id, &record)
            });
            lock(&self.0).busy = false;
            result?;
        }
        Ok(())
    }
    pub fn leave(&self, player: &Player) {
        let state = lock(&self.0);
        if state.players.is_empty() && state.menus.is_empty() {
            return;
        }
        drop(state);
        if let Ok(id) = player_id(&player.get_id().to_string()) {
            let mut state = lock(&self.0);
            state.players.remove(&id);
            state.menus.remove(&id);
        }
    }
}
/// Fixed size: the file stays the truth, so forgetting the oldest id only costs one read later.
fn remember(state: &mut State, id: PlayerId, record: PlayerRecord) {
    if state.players.len() >= MAX_MENUS && !state.players.contains_key(&id) {
        state.players.pop_first();
    }
    state.players.insert(id, record);
}
fn menu_gui(title: &str) -> Gui {
    let gui = Gui::new(Screen::Generic9x6, TextComponent::text(title));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    gui
}
fn button(gui: &Gui, slot: u32, item: &str, player: &Player, key: &str) {
    let icon = ItemStack::new(item, 1);
    icon.set_custom_name(Some(TextComponent::text(&lang::text(
        &player.get_locale(),
        key,
        &[],
    ))));
    gui.set_item(slot, icon);
}
fn list_slot(index: usize) -> u32 {
    match index {
        0 => 11,
        1 => 13,
        _ => (0..45)
            .filter(|slot| ![11, 13].contains(slot))
            .nth(index - 2)
            .unwrap_or(0),
    }
}
fn valid_click(slot: i16, raw_slot: i16, click: ClickType) -> bool {
    (0..54).contains(&raw_slot)
        && slot == raw_slot
        && matches!(click, ClickType::Left | ClickType::Right)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menus_ignore_bottom_inventory_shift_double_number_drop_and_outside_clicks() {
        assert!(!valid_click(11, 65, ClickType::Left));
        assert!(!valid_click(-999, -999, ClickType::Left));
        for click in [
            ClickType::ShiftLeft,
            ClickType::ShiftRight,
            ClickType::DoubleClick,
            ClickType::NumberKey,
            ClickType::Drop,
            ClickType::Middle,
            ClickType::Unknown,
        ] {
            assert!(!valid_click(11, 11, click));
        }
        assert!(valid_click(11, 11, ClickType::Right));
        assert!(valid_click(49, 49, ClickType::Left));
    }
    #[test]
    fn all_page_slots_are_unique_and_never_overlap_controls() {
        let slots: std::collections::BTreeSet<_> = (0..PAGE_SIZE).map(list_slot).collect();
        assert_eq!(slots.len(), PAGE_SIZE);
        assert!(slots.iter().all(|slot| *slot < 45));
        assert_eq!((list_slot(0), list_slot(1)), (11, 13));
    }
    #[test]
    fn settings_validation_and_deletion_preserve_claims() {
        let mut catalog = Catalog::default();
        assert_eq!(
            Change::Cooldown("starter".into(), 0).apply(&mut catalog),
            Err(Error::Invalid)
        );
        Change::Once("starter".into(), true)
            .apply(&mut catalog)
            .unwrap();
        Change::FirstJoin(Some("starter".into()))
            .apply(&mut catalog)
            .unwrap();
        assert!(catalog.kit("starter").unwrap().one_time);
        Change::Delete("starter".into())
            .apply(&mut catalog)
            .unwrap();
        assert_eq!(catalog.first_join, None);
    }

    #[test]
    fn the_player_cache_stays_bounded_without_a_leave_handler() {
        let kits = Kits::default();
        let mut state = lock(&kits.0);
        for index in 0..=MAX_MENUS {
            let mut id = [0; 16];
            id[..8].copy_from_slice(&(index as u64).to_le_bytes());
            remember(&mut state, id, PlayerRecord::default());
        }
        assert_eq!(state.players.len(), MAX_MENUS);
        let mut last = [0; 16];
        last[..8].copy_from_slice(&(MAX_COOLDOWN).to_le_bytes());
        remember(&mut state, last, PlayerRecord::default());
        assert!(state.players.contains_key(&last));
        assert_eq!(state.players.len(), MAX_MENUS);
    }

    #[test]
    fn kits_from_the_settings_file_cannot_be_changed_by_command() {
        let mut catalog = Catalog::default();
        settings::apply(
            &settings::parse("[kit fixed]\nitem=stone").unwrap(),
            &mut catalog,
        )
        .unwrap();
        let before = catalog.clone();
        for change in [
            Change::Delete("fixed".into()),
            Change::Cooldown("fixed".into(), 5),
            Change::Once("fixed".into(), true),
            Change::Edit {
                name: "fixed".into(),
                entries: Vec::new(),
            },
        ] {
            assert_eq!(change.apply(&mut catalog), Err(Error::Defined));
        }
        assert_eq!(catalog, before);
        Change::ResetUses(Some("FIXED".into()))
            .apply(&mut catalog)
            .unwrap();
        assert_ne!(
            catalog.kit("fixed").unwrap().id,
            before.kit("fixed").unwrap().id
        );
    }

    #[test]
    fn concurrent_mutations_have_one_bounded_writer_and_unload_refuses_new_work() {
        let kits = Arc::new(Kits::default());
        lock(&kits.0).active = true;
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let kits = Arc::clone(&kits);
                std::thread::spawn(move || kits.begin_change())
            })
            .collect();
        let successes = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(Result::is_ok)
            .count();
        assert_eq!(successes, 1);
        kits.stop();
        lock(&kits.0).busy = false;
        assert_eq!(kits.begin_change(), Err(Error::Busy));
    }

    #[test]
    fn settings_are_published_only_after_save_and_unload_drops_sessions() {
        let kits = Kits::default();
        let original = kits.catalog();
        {
            let mut state = lock(&kits.0);
            state.active = true;
            state.dir = Some(
                std::env::temp_dir()
                    .join(format!("kits-missing-parent-{}", std::process::id()))
                    .join("child"),
            );
            state.menus.insert(
                [4; 16],
                Menu::Preview {
                    name: "starter".into(),
                },
            );
        }
        assert_eq!(
            kits.commit_change(Change::Delete("starter".into())),
            Err(Error::Storage)
        );
        assert_eq!(*kits.catalog(), *original);
        kits.stop();
        assert!(lock(&kits.0).menus.is_empty());
    }
}
