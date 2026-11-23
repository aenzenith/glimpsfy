//! The language the core speaks: the menu bar menu and the names of history entries that have
//! none of their own. The texts live in `src/locales/*.json`, shared with the pages, so a
//! language is added in one place.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::settings::Language;

/// Every language there is a catalog for, in the order Settings lists them.
const SOURCES: [(&str, &str); 7] = [
    ("en", include_str!("../../src/locales/en.json")),
    ("tr", include_str!("../../src/locales/tr.json")),
    ("de", include_str!("../../src/locales/de.json")),
    ("es", include_str!("../../src/locales/es.json")),
    ("ru", include_str!("../../src/locales/ru.json")),
    ("ja", include_str!("../../src/locales/ja.json")),
    ("zh-Hans", include_str!("../../src/locales/zh-Hans.json")),
];

type Catalog = HashMap<String, String>;

fn catalogs() -> &'static HashMap<&'static str, Catalog> {
    static CATALOGS: OnceLock<HashMap<&'static str, Catalog>> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        SOURCES.iter().map(|(code, raw)| (*code, serde_json::from_str(raw).unwrap_or_default())).collect()
    })
}

/// The first of the preferred languages (`tr-TR`, `zh-Hans-CN`, `pt_BR`) there is a catalog for;
/// English when there is none. `src/lib/i18n.ts` picks the same way for the pages.
fn pick(tags: impl IntoIterator<Item = String>) -> &'static str {
    for tag in tags {
        let tag = tag.replace('_', "-").to_lowercase();
        let parts: Vec<&str> = tag.split('-').collect();
        let primary = parts.first().copied().unwrap_or_default();
        if primary == "zh" {
            // Traditional Chinese (Taiwan, Hong Kong, Macau) has no catalog yet.
            let traditional = parts.iter().any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"));
            if traditional && !parts.contains(&"hans") {
                continue;
            }
            return "zh-Hans";
        }
        if let Some((code, _)) = SOURCES.iter().find(|(code, _)| *code == primary) {
            return code;
        }
    }
    "en"
}

/// The system's language, read once: macOS asks apps to restart when it changes.
fn system() -> &'static str {
    static SYSTEM: OnceLock<&'static str> = OnceLock::new();
    SYSTEM.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            let preferred = objc2_foundation::NSLocale::preferredLanguages();
            pick(preferred.iter().map(|l| l.to_string()))
        }
        #[cfg(not(target_os = "macos"))]
        {
            pick(crate::desktop::preferred_languages())
        }
    })
}

/// The catalog code for a Settings choice.
pub fn resolve(choice: Language) -> &'static str {
    match choice {
        Language::System => system(),
        Language::En => "en",
        Language::Tr => "tr",
        Language::De => "de",
        Language::Es => "es",
        Language::Ru => "ru",
        Language::Ja => "ja",
        Language::ZhHans => "zh-Hans",
    }
}

/// The text for `key` in the language of `choice` (English when that catalog lacks it, the key
/// itself when English lacks it too), with `{name}` placeholders filled from `params`.
pub fn text(choice: Language, key: &str, params: &[(&str, &str)]) -> String {
    let catalogs = catalogs();
    let found = catalogs.get(resolve(choice)).and_then(|c| c.get(key)).or_else(|| catalogs.get("en")?.get(key));
    let mut text = found.cloned().unwrap_or_else(|| key.to_string());
    for (name, value) in params {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}
