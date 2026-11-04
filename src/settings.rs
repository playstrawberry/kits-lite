//! The one settings module: `data/kits-settings.txt`, read once when the plugin loads.
//! `key=value` lines first, then `[kit <name>]` and `[text <language>]` sections.
//! A bad line stops the plugin from loading and names the line; nothing is guessed.

use crate::{
    lang,
    model::{Catalog, Entry, Error, MAX_COOLDOWN, MAX_KITS, SLOT_COUNT, Stack, valid_name},
    store,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const FILE: &str = "kits-settings.txt";
const MAX_FILE: usize = 262_144;
const MAX_LABELS: usize = 8;
const MAX_TEXTS: usize = 512;
const MAX_TEXT_BYTES: usize = 512;
const MAX_LINE_COUNT: u8 = 99;
const DEFAULT_COOLDOWN: u64 = 600;
const LANGUAGES: [&str; 4] = ["en", "fr", "es", "tr"];
/// Bundled kits are found by the identity they were created with.
const BUNDLED: [(u64, &str); 2] = [(1, "starter"), (2, "tools")];

const TEMPLATE: &str = "\
# kits-lite settings; they apply when the plugin loads. See the plugin README.
kit-list=text
kits-list=menu
preview=on
menu-claim=on
first-join=on
language=player
unknown-kit=server
bundled-kits=on
kit-labels=
kits-labels=
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListMode {
    Text,
    Menu,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    pub cooldown: u64,
    pub once: bool,
    pub everyone: bool,
    pub items: Vec<(String, u8)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub kit_list: ListMode,
    pub kits_list: ListMode,
    pub preview: bool,
    pub menu_claim: bool,
    pub first_join: bool,
    pub fixed_language: Option<String>,
    pub plugin_unknown_reply: bool,
    pub bundled_kits: bool,
    pub kit_labels: Vec<String>,
    pub kits_labels: Vec<String>,
    pub definitions: Vec<Definition>,
    /// (language, key) to text.
    pub texts: BTreeMap<(String, String), String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            kit_list: ListMode::Text,
            kits_list: ListMode::Menu,
            preview: true,
            menu_claim: true,
            first_join: true,
            fixed_language: None,
            plugin_unknown_reply: false,
            bundled_kits: true,
            kit_labels: Vec::new(),
            kits_labels: Vec::new(),
            definitions: Vec::new(),
            texts: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn has_list_menu(&self) -> bool {
        self.kit_list == ListMode::Menu || self.kits_list == ListMode::Menu
    }
    /// True when any chest menu can open; the inventory handlers exist only then.
    pub fn has_menus(&self) -> bool {
        self.has_list_menu() || self.preview
    }
}

/// The line number (0 for the whole file) and the text that could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub text: String,
}

impl Problem {
    pub fn message(&self) -> String {
        if self.line == 0 {
            return lang::text("en", "settings_value_error", &[&self.text]);
        }
        lang::text(
            "en",
            "settings_error",
            &[&self.line.to_string(), &self.text],
        )
    }
}

enum Section {
    Settings,
    Kit,
    Text(String),
}

pub fn parse(text: &str) -> Result<Settings, Problem> {
    let mut out = Settings::default();
    let mut section = Section::Settings;
    let mut seen = BTreeSet::new();
    let mut open: Option<Partial> = None;
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        let bad = || Problem {
            line: number,
            text: line.chars().take(64).filter(|c| !c.is_control()).collect(),
        };
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(head) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            close(&mut open, &mut out)?;
            let (kind, value) = head.split_once(' ').ok_or_else(bad)?;
            section = match kind {
                "kit" => {
                    let name = value.trim();
                    if !valid_name(name)
                        || out.definitions.len() >= MAX_KITS
                        || out.definitions.iter().any(|kit| kit.name == name)
                    {
                        return Err(bad());
                    }
                    open = Some(Partial {
                        line: number,
                        definition: Definition {
                            name: name.into(),
                            cooldown: DEFAULT_COOLDOWN,
                            once: false,
                            everyone: false,
                            items: Vec::new(),
                        },
                    });
                    Section::Kit
                }
                "text" if LANGUAGES.contains(&value.trim()) => {
                    Section::Text(value.trim().to_string())
                }
                _ => return Err(bad()),
            };
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(bad)?;
        let (key, value) = (key.trim(), value.trim());
        match &section {
            Section::Settings => {
                if !seen.insert(key.to_string()) {
                    return Err(bad());
                }
                setting(&mut out, key, value).ok_or_else(bad)?;
            }
            Section::Kit => {
                let kit = &mut open.as_mut().ok_or_else(bad)?.definition;
                kit_line(kit, key, value).ok_or_else(bad)?;
            }
            Section::Text(language) => {
                if out.texts.len() >= MAX_TEXTS
                    || value.len() > MAX_TEXT_BYTES
                    || value.is_empty()
                    || value.chars().any(char::is_control)
                    || !lang::valid_override(key, value)
                    || out
                        .texts
                        .insert((language.clone(), key.to_string()), value.to_string())
                        .is_some()
                {
                    return Err(bad());
                }
            }
        }
    }
    close(&mut open, &mut out)?;
    let (kit, kits) = (&out.kit_labels, &out.kits_labels);
    if kit.iter().any(|label| kits.contains(label)) {
        return Err(Problem {
            line: 0,
            text: "kit-labels, kits-labels".into(),
        });
    }
    Ok(out)
}

struct Partial {
    line: usize,
    definition: Definition,
}

fn close(open: &mut Option<Partial>, out: &mut Settings) -> Result<(), Problem> {
    if let Some(kit) = open.take() {
        if kit.definition.items.is_empty() {
            return Err(Problem {
                line: kit.line,
                text: kit.definition.name,
            });
        }
        out.definitions.push(kit.definition);
    }
    Ok(())
}

fn setting(out: &mut Settings, key: &str, value: &str) -> Option<()> {
    match key {
        "kit-list" => out.kit_list = list_mode(value)?,
        "kits-list" => out.kits_list = list_mode(value)?,
        "preview" => out.preview = switch(value)?,
        "menu-claim" => out.menu_claim = switch(value)?,
        "first-join" => out.first_join = switch(value)?,
        "bundled-kits" => out.bundled_kits = switch(value)?,
        "language" => {
            out.fixed_language = match value {
                "player" => None,
                code if LANGUAGES.contains(&code) => Some(code.to_string()),
                _ => return None,
            }
        }
        "unknown-kit" => {
            out.plugin_unknown_reply = match value {
                "server" => false,
                "plugin" => true,
                _ => return None,
            }
        }
        "kit-labels" => out.kit_labels = labels(value)?,
        "kits-labels" => out.kits_labels = labels(value)?,
        _ => return None,
    }
    Some(())
}

fn list_mode(value: &str) -> Option<ListMode> {
    match value {
        "text" => Some(ListMode::Text),
        "menu" => Some(ListMode::Menu),
        _ => None,
    }
}

fn switch(value: &str) -> Option<bool> {
    match value {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

fn labels(value: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for label in value.split(',').map(str::trim).filter(|l| !l.is_empty()) {
        if !valid_label(label) || out.iter().any(|known| known == label) {
            return None;
        }
        out.push(label.to_string());
    }
    (out.len() <= MAX_LABELS).then_some(out)
}

fn valid_label(label: &str) -> bool {
    label.len() <= 32
        && label
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
        && !["kit", "kits", "kitadmin", "marketplace"].contains(&label)
        && !label.starts_with("strawberry")
        && !label.starts_with("playstrawberry")
}

fn kit_line(kit: &mut Definition, key: &str, value: &str) -> Option<()> {
    match key {
        "cooldown" => {
            kit.cooldown = value
                .parse()
                .ok()
                .filter(|seconds| (1..=MAX_COOLDOWN).contains(seconds))?;
        }
        "once" => kit.once = bool_word(value)?,
        "access" => {
            kit.everyone = match value {
                "restricted" => false,
                "everyone" => true,
                _ => return None,
            }
        }
        "item" => {
            let mut fields = value.split_ascii_whitespace();
            let key = item_key(fields.next()?)?;
            let count = match fields.next() {
                Some(count) => count.parse::<u8>().ok()?,
                None => 1,
            };
            if fields.next().is_some()
                || !(1..=MAX_LINE_COUNT).contains(&count)
                || kit.items.len() >= SLOT_COUNT
            {
                return None;
            }
            kit.items.push((key, count));
        }
        _ => return None,
    }
    Some(())
}

fn bool_word(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn item_key(key: &str) -> Option<String> {
    if key.is_empty() || key.len() > 128 {
        return None;
    }
    if key.contains(':') {
        Some(key.to_string())
    } else {
        Some(format!("minecraft:{key}"))
    }
}

/// Reads the settings file. When it is missing, the template is written once and used.
pub fn load(dir: &Path) -> Result<Settings, Problem> {
    let whole = |text: &str| Problem {
        line: 0,
        text: text.into(),
    };
    let path = dir.join(FILE);
    match store::read_bounded(&path, MAX_FILE).map_err(|_| whole(FILE))? {
        Some(bytes) => parse(std::str::from_utf8(&bytes).map_err(|_| whole(FILE))?),
        None => {
            // A read-only folder still runs on the defaults below.
            let _ = std::fs::write(&path, TEMPLATE);
            parse(TEMPLATE)
        }
    }
}

/// Brings the stored catalog in line with the file. Identities stay, so uses are kept.
pub fn apply(settings: &Settings, catalog: &mut Catalog) -> Result<(), Error> {
    let names: BTreeSet<&str> = settings
        .definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    let gone: Vec<String> = catalog
        .kits
        .iter()
        .filter(|kit| {
            let bundled = BUNDLED.contains(&(kit.id, kit.name.as_str()));
            !names.contains(kit.name.as_str())
                && (kit.listed || (bundled && !settings.bundled_kits))
        })
        .map(|kit| kit.name.clone())
        .collect();
    for name in gone {
        catalog.delete(&name)?;
    }
    for definition in &settings.definitions {
        let entries: Vec<Entry> = definition
            .items
            .iter()
            .enumerate()
            .map(|(slot, (key, count))| Entry {
                slot: slot as u8,
                stack: Stack {
                    key: key.clone(),
                    count: *count,
                    data: Vec::new(),
                },
            })
            .collect();
        if let Ok(kit) = catalog.kit(&definition.name) {
            // Captured item data has no readable form: never replace it by accident.
            let captured = kit.entries.iter().any(|entry| !entry.stack.data.is_empty());
            if captured && !kit.listed {
                return Err(Error::Exists);
            }
        } else {
            catalog.create(
                &definition.name,
                entries.clone(),
                definition.cooldown,
                definition.once,
            )?;
        }
        let kit = catalog
            .kits
            .iter_mut()
            .find(|kit| kit.name == definition.name)
            .ok_or(Error::Unknown)?;
        kit.entries = entries;
        kit.cooldown = definition.cooldown;
        kit.one_time = definition.once;
        kit.restricted = !definition.everyone;
        kit.listed = true;
    }
    catalog.validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::item;

    const COMMAND_ONLY: &str = "
kit-list=text
kits-list=text
preview=off
menu-claim=off
first-join=off
language=tr
unknown-kit=plugin
bundled-kits=off
kit-labels=ekit
kits-labels=ekits

[kit oyuncu]
cooldown=860
item=minecraft:stone_sword 1
item=cooked_beef 16

[text tr]
claimed={0} geldi.
";

    fn problem(text: &str) -> usize {
        parse(text).unwrap_err().line
    }

    #[test]
    fn defaults_keep_every_feature_on_and_add_no_kit() {
        let settings = parse("").unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(parse(TEMPLATE), Ok(Settings::default()));
        assert!(settings.has_menus());
        assert!(settings.first_join);
        assert!(!settings.plugin_unknown_reply);
    }

    #[test]
    fn a_command_only_profile_turns_off_every_menu_and_automatic_path() {
        let settings = parse(COMMAND_ONLY).unwrap();
        assert!(!settings.has_menus());
        assert!(!settings.first_join);
        assert!(!settings.menu_claim);
        assert!(!settings.bundled_kits);
        assert!(settings.plugin_unknown_reply);
        assert_eq!(settings.fixed_language.as_deref(), Some("tr"));
        assert_eq!(settings.kit_labels, ["ekit"]);
        assert_eq!(settings.kits_labels, ["ekits"]);
        let kit = &settings.definitions[0];
        assert_eq!((kit.name.as_str(), kit.cooldown), ("oyuncu", 860));
        assert!(
            !kit.everyone,
            "a defined kit is restricted unless it says so"
        );
        assert_eq!(
            kit.items,
            [
                ("minecraft:stone_sword".to_string(), 1),
                ("minecraft:cooked_beef".to_string(), 16)
            ]
        );
        assert_eq!(
            settings.texts[&("tr".to_string(), "claimed".to_string())],
            "{0} geldi."
        );
    }

    #[test]
    fn a_preview_alone_keeps_the_menu_handlers_but_no_list_menu() {
        let settings = parse("kit-list=text\nkits-list=text\npreview=on").unwrap();
        assert!(settings.has_menus());
        assert!(!settings.has_list_menu());
    }

    #[test]
    fn bad_lines_are_refused_and_named() {
        assert_eq!(problem("nonsense"), 1);
        assert_eq!(problem("\nnot-a-setting=1"), 2);
        assert_eq!(problem("preview=maybe"), 1);
        assert_eq!(problem("preview=on\npreview=off"), 2);
        assert_eq!(problem("language=de"), 1);
        assert_eq!(problem("[other x]"), 1);
        assert_eq!(problem("[kit Bad]\nitem=stone"), 1);
        assert_eq!(problem("[kit create]\nitem=stone"), 1);
        assert_eq!(problem("[kit a]\n[kit b]\nitem=stone"), 1);
        assert_eq!(problem("[kit a]\nitem=stone\n[kit a]\nitem=stone"), 3);
        assert_eq!(problem("[kit a]\ncooldown=0\nitem=stone"), 2);
        assert_eq!(problem("[kit a]\nitem=stone 0"), 2);
        assert_eq!(problem("[kit a]\nitem=stone 100"), 2);
        assert_eq!(problem("[kit a]\nitem=stone 1 2"), 2);
        assert_eq!(problem("[kit a]\nprice=4"), 2);
        let many: String = (0..=SLOT_COUNT).map(|_| "item=stone\n").collect();
        assert_eq!(problem(&format!("[kit a]\n{many}")), SLOT_COUNT + 2);
    }

    #[test]
    fn labels_cannot_shadow_reserved_commands_or_repeat() {
        for label in [
            "kit",
            "kits",
            "kitadmin",
            "marketplace",
            "strawberry",
            "strawberry-x",
            "playstrawberry",
            "Upper",
            "a:b",
        ] {
            assert_eq!(problem(&format!("kit-labels={label}")), 1, "{label}");
        }
        assert_eq!(problem("kit-labels=a,a"), 1);
        assert_eq!(parse("kit-labels=a\nkits-labels=a").unwrap_err().line, 0);
        let many: Vec<String> = (0..=MAX_LABELS).map(|i| format!("l{i}")).collect();
        assert_eq!(problem(&format!("kit-labels={}", many.join(","))), 1);
    }

    #[test]
    fn text_overrides_must_match_known_keys_and_placeholders() {
        assert!(
            parse("[text tr]\nclaimed=Tamam.").is_ok(),
            "may drop an argument"
        );
        assert_eq!(problem("[text de]\nclaimed=x"), 1);
        assert_eq!(problem("[text tr]\nnot_a_key=x"), 2);
        assert_eq!(problem("[text tr]\nclaimed={1}"), 2);
        assert_eq!(problem("[text tr]\nclaimed={0"), 2);
        assert_eq!(problem("[text tr]\nunknown_command=x"), 2);
        assert_eq!(problem("[text tr]\nclaimed=a\nclaimed=b"), 3);
        let long = "x".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(problem(&format!("[text tr]\nclaimed={long}")), 2);
    }

    #[test]
    fn definitions_keep_identity_across_loads_and_removing_one_deletes_it() {
        let settings = parse(COMMAND_ONLY).unwrap();
        let mut catalog = Catalog::default();
        apply(&settings, &mut catalog).unwrap();
        assert!(catalog.kit("starter").is_err() && catalog.kit("tools").is_err());
        let kit = catalog.kit("oyuncu").unwrap().clone();
        assert!(kit.restricted && kit.listed);
        assert_eq!(kit.cooldown, 860);
        assert_eq!(kit.entries[1].stack.count, 16);

        let again = catalog.clone();
        apply(&settings, &mut catalog).unwrap();
        assert_eq!(catalog, again, "a second load changes nothing");

        let edited = COMMAND_ONLY.replace("cooldown=860", "cooldown=900");
        apply(&parse(&edited).unwrap(), &mut catalog).unwrap();
        assert_eq!(catalog.kit("oyuncu").unwrap().id, kit.id);
        assert_eq!(catalog.kit("oyuncu").unwrap().cooldown, 900);

        apply(&Settings::default(), &mut catalog).unwrap();
        assert!(catalog.kit("oyuncu").is_err());
    }

    #[test]
    fn a_definition_never_replaces_captured_item_data_and_may_replace_a_bundled_kit() {
        let mut catalog = Catalog::default();
        catalog.create("daily", vec![item(0)], 10, false).unwrap();
        let settings = parse("[kit daily]\nitem=stone").unwrap();
        assert_eq!(apply(&settings, &mut catalog), Err(Error::Exists));

        let mut catalog = Catalog::default();
        let settings = parse("bundled-kits=off\n[kit starter]\nitem=stone").unwrap();
        apply(&settings, &mut catalog).unwrap();
        let kit = catalog.kit("starter").unwrap();
        assert_eq!((kit.id, kit.listed, kit.restricted), (1, true, true));
        assert!(catalog.kit("tools").is_err());
    }

    #[test]
    fn turning_bundled_kits_off_leaves_a_captured_kit_with_the_same_name_alone() {
        let mut catalog = Catalog::default();
        catalog.delete("tools").unwrap();
        catalog.create("tools", vec![item(0)], 10, false).unwrap();
        apply(&parse("bundled-kits=off").unwrap(), &mut catalog).unwrap();
        assert!(catalog.kit("starter").is_err());
        assert!(catalog.kit("tools").is_ok());
    }
}
