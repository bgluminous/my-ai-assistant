use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager};

use crate::{paths, settings, tray};

type Messages = BTreeMap<String, String>;

#[derive(Deserialize)]
struct Pack {
    locale: String,
    name: String,
    messages: Messages,
}

#[derive(Clone, Serialize)]
pub struct Choice {
    locale: String,
    name: String,
}

struct Library {
    packs: BTreeMap<String, Pack>,
}

const BUILTINS: &[&str] = include!(concat!(env!("OUT_DIR"), "/language_packs.rs"));

fn language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-').peekable();
    let letters = |s: &str| s.bytes().all(|b| b.is_ascii_alphabetic());
    let primary = parts.next().unwrap_or("");
    if !(2..=3).contains(&primary.len()) || !letters(primary) {
        return false;
    }
    if parts.peek().is_some_and(|p| p.len() == 4 && letters(p)) {
        parts.next();
    }
    if let Some(region) = parts.next() {
        if !(region.len() == 2 && letters(region)
            || region.len() == 3 && region.bytes().all(|b| b.is_ascii_digit()))
        {
            return false;
        }
    }
    parts.next().is_none()
}

fn placeholders(value: &str) -> BTreeSet<&str> {
    value
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(index, _)| index))
        .filter(|index| !index.is_empty() && index.bytes().all(|c| c.is_ascii_digit()))
        .collect()
}

impl Library {
    fn load(directory: Option<&Path>) -> Self {
        let mut packs = BTreeMap::new();
        for source in BUILTINS {
            let pack: Pack = serde_json::from_str(source).expect("bundled language pack");
            packs.insert(pack.locale.clone(), pack);
        }
        let defaults = packs["en"].messages.clone();
        if let Some(entries) = directory.and_then(|path| std::fs::read_dir(path).ok()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let Some(mut custom) = std::fs::read(&path)
                    .ok()
                    .and_then(|data| serde_json::from_slice::<Pack>(&data).ok())
                else {
                    continue;
                };
                if path.file_stem().and_then(|s| s.to_str()) != Some(&custom.locale)
                    || !language_tag(&custom.locale)
                    || custom.name.trim().is_empty()
                {
                    continue;
                }
                custom.messages.retain(|key, value| {
                    defaults.get(key).is_some_and(|base| {
                        !value.is_empty() && placeholders(base) == placeholders(value)
                    })
                });
                let mut messages = packs
                    .get(&custom.locale)
                    .map_or(&defaults, |p| &p.messages)
                    .clone();
                messages.append(&mut custom.messages);
                custom.messages = messages;
                packs.insert(custom.locale.clone(), custom);
            }
        }
        Self { packs }
    }

    fn resolve(
        &self,
        preference: &str,
        locales: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Language {
        let supported = |tag: &str| {
            self.packs
                .keys()
                .find(|k| k.eq_ignore_ascii_case(tag))
                .cloned()
        };
        if let Some(locale) = supported(preference) {
            return Language {
                preference: locale.clone(),
                locale,
            };
        }
        for item in locales {
            let tag = item.as_ref().replace('_', "-");
            let primary = tag.split('-').next().unwrap_or("");
            let mut candidate = tag.as_str();
            loop {
                if let Some(locale) = supported(candidate) {
                    return Language::system(locale);
                }
                let Some((parent, _)) = candidate.rsplit_once('-') else {
                    break;
                };
                candidate = parent;
            }
            if primary.eq_ignore_ascii_case("zh") {
                return Language::system("zh-CN".into());
            }
            if let Some(key) = self.packs.keys().find(|key| {
                key.split('-')
                    .next()
                    .is_some_and(|base| base.eq_ignore_ascii_case(primary))
            }) {
                return Language::system(key.clone());
            }
        }
        Language::system("en".into())
    }
}

fn library() -> &'static Library {
    static LIBRARY: OnceLock<Library> = OnceLock::new();
    LIBRARY.get_or_init(|| Library::load(paths::app_dir().map(|p| p.join("locales")).as_deref()))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Language {
    preference: String,
    locale: String,
}

impl Language {
    fn system(locale: String) -> Self {
        Self {
            preference: "system".into(),
            locale,
        }
    }

    pub fn text(&self, key: &str) -> &'static str {
        let packs = &library().packs;
        packs[&self.locale]
            .messages
            .get(key)
            .or_else(|| packs["en"].messages.get(key))
            .map(String::as_str)
            .unwrap_or("")
    }

    fn set_titles(&self, app: &AppHandle) {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_title(self.text("app.title"));
        }
        if let Some(window) = app.get_webview_window("tray") {
            let _ = window.set_title(self.text("tray.title"));
        }
    }

    fn view(&self) -> View {
        let packs = &library().packs;
        let languages = packs
            .values()
            .map(|p| Choice {
                locale: p.locale.clone(),
                name: p.name.clone(),
            })
            .collect();
        View {
            language: self.clone(),
            languages,
            messages: &packs[&self.locale].messages,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct View {
    #[serde(flatten)]
    language: Language,
    languages: Vec<Choice>,
    messages: &'static Messages,
}

pub fn builtin_text(key: &str) -> &'static str {
    static SOURCE: OnceLock<Pack> = OnceLock::new();
    SOURCE
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../src/locales/zh-CN.json"))
                .expect("Chinese language pack")
        })
        .messages
        .get(key)
        .map(String::as_str)
        .unwrap_or("")
}

pub fn current() -> Language {
    let preference = settings::read(|s| s.ui_language.clone())
        .ok()
        .flatten()
        .unwrap_or_default();
    library().resolve(&preference, sys_locale::get_locales())
}

pub fn initialize(app: &AppHandle) -> Language {
    let language = current();
    language.set_titles(app);
    app.manage(AppliedLanguage(Mutex::new(language.clone())));
    language
}

struct AppliedLanguage(Mutex<Language>);

pub fn publish(app: &AppHandle) -> Language {
    let language = current();
    let Some(applied) = app.try_state::<AppliedLanguage>() else {
        return language;
    };
    if *applied.0.lock().unwrap_or_else(|e| e.into_inner()) == language {
        return language;
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let latest = current();
        let state = handle.state::<AppliedLanguage>();
        {
            let mut previous = state.0.lock().unwrap_or_else(|e| e.into_inner());
            if *previous == latest {
                return;
            }
            *previous = latest.clone();
        }
        latest.set_titles(&handle);
        tray::update_language(&handle, &latest);
        let _ = handle.emit("ui-language-changed", latest.view());
    });
    language
}

#[tauri::command]
pub fn ui_language_get(app: AppHandle) -> Result<View, String> {
    settings::ensure_loaded()?;
    Ok(publish(&app).view())
}

#[tauri::command]
pub fn ui_language_open_folder() -> Result<(), String> {
    let directory = paths::app_dir()
        .ok_or("home_dir_unavailable")?
        .join("locales");
    let templates = directory.join("templates").join(env!("CARGO_PKG_VERSION"));
    std::fs::create_dir_all(&templates).map_err(|e| e.to_string())?;
    for source in BUILTINS {
        let pack: Pack = serde_json::from_str(source).map_err(|e| e.to_string())?;
        let path = templates.join(format!("{}.json", pack.locale));
        if path.exists() {
            continue;
        }
        let mut file = tempfile::NamedTempFile::new_in(&templates).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut file, source.as_bytes()).map_err(|e| e.to_string())?;
        if let Err(error) = file.persist_noclobber(path) {
            if error.error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.error.to_string());
            }
        }
    }
    let opener = if cfg!(windows) {
        "explorer.exe"
    } else {
        "open"
    };
    std::process::Command::new(opener)
        .arg(directory)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn ui_language_set(app: AppHandle, preference: String) -> Result<View, String> {
    if preference != "system" && !library().packs.contains_key(&preference) {
        return Err("invalid_ui_language".into());
    }
    settings::mutate(|s| {
        s.ui_language = Some(preference);
        Ok(())
    })?;
    Ok(publish(&app).view())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_packs_match_system_preferences_and_accept_partial_translations() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fr.json"), r#"{"locale":"fr","name":"Français","messages":{"tray.quit":"Quitter","time.minutesAgo":"missing argument"}}"#).unwrap();
        std::fs::write(
            dir.path().join("en.json"),
            r#"{"locale":"en","name":"English","messages":{"tray.quit":"Exit"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("ja.json"), "invalid").unwrap();
        let lib = Library::load(Some(dir.path()));
        assert_eq!(lib.resolve("system", ["de", "fr-CA", "en"]).locale, "fr");
        assert_eq!(
            lib.resolve("system", ["zh-Hant-TW", "en-US"]).locale,
            "zh-CN"
        );
        assert_eq!(lib.resolve("system", ["en-GB", "zh-CN"]).locale, "en");
        assert_eq!(lib.resolve("system", ["ja"]).locale, "en");
        assert_eq!(lib.resolve("zh-CN", ["en"]).locale, "zh-CN");
        assert_eq!(lib.packs["fr"].messages["tray.quit"], "Quitter");
        assert_eq!(lib.packs["en"].messages["tray.quit"], "Exit");
        assert_eq!(
            lib.packs["fr"].messages["time.minutesAgo"],
            lib.packs["en"].messages["time.minutesAgo"]
        );
    }
}
