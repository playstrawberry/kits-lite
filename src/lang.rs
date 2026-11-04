//! Compiled message catalogs; unknown Minecraft locales use English.

use crate::model::Error;
use std::{collections::BTreeMap, sync::RwLock};

const FILES: [(&str, &str); 4] = [
    ("en", include_str!("../lang/en.lang")),
    ("fr", include_str!("../lang/fr.lang")),
    ("es", include_str!("../lang/es.lang")),
    ("tr", include_str!("../lang/tr.lang")),
];

fn lookup<'a>(file: &'a str, key: &str) -> Option<&'a str> {
    file.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value)
}

/// What the settings file chose; read on every message, written only when the plugin loads.
struct Choices {
    fixed: Option<String>,
    texts: BTreeMap<(String, String), String>,
}

static CHOICES: RwLock<Choices> = RwLock::new(Choices {
    fixed: None,
    texts: BTreeMap::new(),
});

pub fn configure(fixed: Option<String>, texts: BTreeMap<(String, String), String>) {
    *CHOICES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Choices { fixed, texts };
}

/// True when a server owner's text fits this key: known, and only its own placeholders.
pub fn valid_override(key: &str, value: &str) -> bool {
    if key == "unknown_command" {
        return false;
    }
    let Some(english) = lookup(FILES[0].1, key) else {
        return false;
    };
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            return false;
        };
        let placeholder = &rest[start..=start + end];
        if !english.contains(placeholder) {
            return false;
        }
        rest = &rest[start + end + 1..];
    }
    true
}

fn resolve<'a>(choices: &'a Choices, locale: &str, key: &'a str) -> &'a str {
    let asked = locale.get(..2).unwrap_or("").to_ascii_lowercase();
    let code = choices.fixed.as_deref().unwrap_or(&asked);
    let (language, file) = FILES
        .iter()
        .find(|(language, _)| *language == code)
        .map_or(FILES[0], |entry| *entry);
    choices
        .texts
        .get(&(language.to_string(), key.to_string()))
        .map(String::as_str)
        .or_else(|| lookup(file, key))
        .or_else(|| lookup(FILES[0].1, key))
        .unwrap_or(key)
}

pub fn text(locale: &str, key: &str, args: &[&str]) -> String {
    let choices = CHOICES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    format(resolve(&choices, locale, key), args)
}

fn format(raw: &str, args: &[&str]) -> String {
    let mut out =
        String::with_capacity(raw.len() + args.iter().map(|arg| arg.len()).sum::<usize>());
    let mut rest = raw;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(end) = rest.find('}')
            && let Ok(index) = rest[1..end].parse::<usize>()
            && let Some(arg) = args.get(index)
        {
            out.push_str(arg);
            rest = &rest[end + 1..];
            continue;
        }
        out.push('{');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

pub fn error(locale: &str, error: &Error, kit: &str) -> String {
    let key = match error {
        Error::Invalid => "invalid",
        Error::Unknown => "unknown",
        Error::Exists => "exists",
        Error::Limit => "limit",
        Error::Empty => "empty",
        Error::Full => "full",
        Error::Busy => "busy",
        Error::Cooldown(seconds) => return text(locale, "cooldown", &[kit, &seconds.to_string()]),
        Error::Once => "once",
        #[cfg(test)]
        Error::Pending => "pending",
        Error::Storage => "storage",
        Error::Permission => "permission",
        Error::PlayerOnly => "player_only",
        Error::Offline => "offline",
        Error::Defined => "defined",
        Error::Disabled => "disabled",
        Error::Partial => "partial",
        Error::NoImport => "no_import",
    };
    text(locale, key, &[kit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn catalog(file: &str) -> BTreeMap<&str, &str> {
        let rows: Vec<_> = file
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.split_once('=').unwrap())
            .collect();
        let map: BTreeMap<_, _> = rows.iter().copied().collect();
        assert_eq!(rows.len(), map.len(), "duplicate translation key");
        map
    }

    fn placeholders(value: &str) -> BTreeSet<&str> {
        value
            .split('{')
            .skip(1)
            .map(|part| part.split_once('}').unwrap().0)
            .collect()
    }

    #[test]
    fn every_language_has_exact_keys_nonempty_values_and_placeholders() {
        let english = catalog(FILES[0].1);
        for (_, file) in FILES {
            let translated = catalog(file);
            assert_eq!(
                english.keys().collect::<Vec<_>>(),
                translated.keys().collect::<Vec<_>>()
            );
            for (key, value) in translated {
                assert!(!value.trim().is_empty());
                assert_eq!(placeholders(value), placeholders(english[key]), "{key}");
            }
        }
    }

    fn choices(fixed: Option<&str>, texts: &[(&str, &str, &str)]) -> Choices {
        Choices {
            fixed: fixed.map(str::to_string),
            texts: texts
                .iter()
                .map(|(language, key, value)| {
                    (
                        ((*language).to_string(), (*key).to_string()),
                        (*value).to_string(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn a_fixed_language_replaces_the_player_locale() {
        let tr = lookup(FILES[3].1, "ready").unwrap();
        let en = lookup(FILES[0].1, "ready").unwrap();
        assert_eq!(resolve(&choices(Some("tr"), &[]), "en_us", "ready"), tr);
        assert_eq!(resolve(&choices(None, &[]), "en_us", "ready"), en);
        assert_eq!(resolve(&choices(None, &[]), "tr_tr", "ready"), tr);
    }

    #[test]
    fn owner_texts_win_for_their_language_and_everything_else_falls_back() {
        let set = choices(None, &[("tr", "ready", "tamam")]);
        assert_eq!(resolve(&set, "tr_tr", "ready"), "tamam");
        assert_eq!(
            resolve(&set, "tr_tr", "used"),
            lookup(FILES[3].1, "used").unwrap()
        );
        assert_eq!(
            resolve(&set, "fr_fr", "ready"),
            lookup(FILES[1].1, "ready").unwrap()
        );
        let english = choices(None, &[("en", "ready", "go")]);
        assert_eq!(resolve(&english, "de_de", "ready"), "go");
    }

    #[test]
    fn overrides_may_use_only_their_keys_own_placeholders() {
        assert!(valid_override("claimed", "{0} geldi"));
        assert!(valid_override("claimed", "tamam"));
        assert!(valid_override("given", "{1} {0}"));
        assert!(!valid_override("claimed", "{1}"));
        assert!(!valid_override("claimed", "{0"));
        assert!(!valid_override("nothing", "x"));
        assert!(!valid_override("unknown_command", "x"));
    }

    #[test]
    fn unsupported_locales_and_utf8_boundaries_are_safe_and_arguments_are_literal() {
        assert_eq!(text("é", "ready", &[]), "ready");
        assert_eq!(text("de_de", "ready", &[]), "ready");
        assert_ne!(text("TR_tr", "ready", &[]), "ready");
        assert_eq!(text("en", "saved", &["{1}"]), "Kit {1} saved.");
    }
}
