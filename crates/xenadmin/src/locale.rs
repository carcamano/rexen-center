//! The message catalogue: one mechanism for every user-visible string.
//!
//! Each area of the UI declares its strings with [`string_table!`]: a key
//! (the field name), the English text, and doc comments naming the
//! `{placeholders}` the text takes. A table lives in a namespace (`main`,
//! `ops`, `prov`…), so a string's catalogue key is `namespace.key`, for
//! example `main.menu_file`. Code reads strings through the table's typed
//! accessor (`strings().menu_file`), so a missing key is a compile error.
//!
//! English is compiled in and is the fallback of every other language. A
//! translation is a TOML catalogue with one section per namespace
//! (`locales/en.toml` is the exported English one, the template to copy),
//! registered in [`LOCALES`]. Keys it lacks fall back to English; unknown
//! keys and placeholders that differ from the English ones are reported by
//! [`validate`] (a test runs it over every shipped catalogue). See
//! `locales/README.md` for the steps to add pt-BR.
//!
//! The active language is process-wide ([`set_active`]), chosen in the
//! Options dialog (or "follow the system") and saved in the settings. It
//! takes effect at the next frame.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The most languages [`LOCALES`] may list (the size of each table's
/// per-language cache).
pub const MAX_LOCALES: usize = 8;

/// A language the console ships.
pub struct LocaleInfo {
    /// BCP 47 tag (`en`, `pt-BR`); also the value saved in the settings.
    pub tag: &'static str,
    /// The language's own name, as listed in the Options dialog.
    pub name: &'static str,
    source: Source,
}

enum Source {
    /// The compiled-in English tables.
    English,
    /// A TOML catalogue (`include_str!("../locales/<tag>.toml")`).
    #[allow(dead_code)] // No translation ships yet (plan decision 5).
    Toml(&'static str),
    /// Test-only pseudo-language: every English string wrapped in `[[…]]`.
    #[cfg(test)]
    Pseudo,
}

/// The shipped languages; the first is the fallback. To add one, put its
/// catalogue in `locales/` and list it here, e.g.
/// `LocaleInfo { tag: "pt-BR", name: "Português (Brasil)",
/// source: Source::Toml(include_str!("../locales/pt-BR.toml")) }`.
#[cfg(not(test))]
pub static LOCALES: &[LocaleInfo] = &[ENGLISH];

/// Under test, a pseudo-language proves that every table follows the
/// active language without shipping a translation.
#[cfg(test)]
pub static LOCALES: &[LocaleInfo] = &[
    ENGLISH,
    LocaleInfo {
        tag: "en-XA",
        name: "Pseudo",
        source: Source::Pseudo,
    },
];

const ENGLISH: LocaleInfo = LocaleInfo {
    tag: "en",
    name: "English",
    source: Source::English,
};

const _: () = assert!(LOCALES.len() <= MAX_LOCALES);

/// One table's keys and English texts, for export and validation.
pub struct TableInfo {
    pub namespace: &'static str,
    /// A [`string_table!`]'s `(key, English text, doc lines)`.
    pub fields: &'static [(&'static str, &'static str, &'static [&'static str])],
    /// A data table's `(key, English text)` (the original's friendly error
    /// and alert names, looked up by XAPI code).
    pub pairs: &'static [(&'static str, &'static str)],
}

impl TableInfo {
    /// `(key, English text, doc lines)` of every entry.
    pub fn entries(
        &self,
    ) -> impl Iterator<Item = (&'static str, &'static str, &'static [&'static str])> {
        let fields = self.fields.iter().copied();
        let pairs = self
            .pairs
            .iter()
            .map(|&(k, en)| (k, en, &[] as &'static [&'static str]));
        fields.chain(pairs)
    }
}

/// The original's friendly XAPI error texts (`{0}`… are the error's
/// parameters).
pub static ERRORS: TableInfo = TableInfo {
    namespace: "errors",
    fields: &[],
    pairs: crate::friendly_errors::FRIENDLY_ERRORS,
};

/// The titles of XAPI messages (alerts).
pub static MESSAGE_NAMES: TableInfo = TableInfo {
    namespace: "message_names",
    fields: &[],
    pairs: crate::message_names::MESSAGE_NAMES,
};

/// The descriptions of XAPI messages (`{0}`: the object, `{1}`: the pool).
pub static MESSAGE_BODIES: TableInfo = TableInfo {
    namespace: "message_bodies",
    fields: &[],
    pairs: crate::message_names::MESSAGE_BODIES,
};

/// The active language's text for an entry of a data table ([`ERRORS`],
/// [`MESSAGE_NAMES`], [`MESSAGE_BODIES`]); `None`: use the English one.
pub fn translated(table: &TableInfo, key: &str) -> Option<&'static str> {
    catalogue(active_index())?.get(table.namespace, key)
}

/// Every table of the console, in catalogue order.
pub fn tables() -> [&'static TableInfo; 20] {
    [
        &crate::i18n::TABLE,
        &crate::ops_strings::TABLE,
        &crate::tab_strings::TABLE,
        &crate::perf_strings::TABLE,
        &crate::prov_strings::TABLE,
        &crate::sr_strings::TABLE,
        &crate::net_strings::TABLE,
        &crate::pool_strings::TABLE,
        &crate::ha_strings::TABLE,
        &crate::xva_strings::TABLE,
        &crate::ovf_strings::TABLE,
        &crate::cpm_strings::TABLE,
        &crate::vapp_strings::TABLE,
        &crate::vmss_strings::TABLE,
        &crate::dr_strings::TABLE,
        &crate::org_strings::TABLE,
        &crate::dev_strings::TABLE,
        &ERRORS,
        &MESSAGE_NAMES,
        &MESSAGE_BODIES,
    ]
}

// ---- the active language ----------------------------------------------------

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
thread_local! {
    /// Tests switch language on their own thread only, so tests running in
    /// parallel keep English.
    static TEST_ACTIVE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn active_index() -> usize {
    #[cfg(test)]
    if let Some(i) = TEST_ACTIVE.with(|c| c.get()) {
        return i;
    }
    ACTIVE.load(Ordering::Relaxed)
}

/// The language in use.
#[cfg(test)]
pub fn active() -> &'static LocaleInfo {
    &LOCALES[active_index()]
}

/// Uses `tag` from now on; `false` (and no change) when it is not shipped.
/// Under test only the calling thread switches, so tests running in
/// parallel keep English.
pub fn set_active(tag: &str) -> bool {
    match LOCALES.iter().position(|l| l.tag == tag) {
        Some(i) => {
            #[cfg(test)]
            TEST_ACTIVE.with(|c| c.set(Some(i)));
            #[cfg(not(test))]
            ACTIVE.store(i, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// Uses the language of the setting (`""`: follow the system).
pub fn apply_preference(setting: &str) {
    let tag = resolve(setting, system_languages());
    set_active(tag);
    tracing::info!(setting, language = tag, "user interface language");
}

/// Switches the calling thread's language (tests only).
#[cfg(test)]
pub fn set_active_for_this_thread(tag: Option<&str>) {
    let i = tag.map(|t| {
        LOCALES
            .iter()
            .position(|l| l.tag == t)
            .expect("shipped language")
    });
    TEST_ACTIVE.with(|c| c.set(i));
}

/// The shipped language for a setting: the one it names, or for `""` the
/// first of the system's preferred languages that ships (exact tag first,
/// then the same primary language), else English.
pub fn resolve(setting: &str, system: &[String]) -> &'static str {
    if !setting.is_empty() {
        if let Some(l) = LOCALES.iter().find(|l| l.tag.eq_ignore_ascii_case(setting)) {
            return l.tag;
        }
        return LOCALES[0].tag;
    }
    for wanted in system {
        let wanted = wanted.replace('_', "-");
        // `pt-BR.UTF-8` → `pt-BR`.
        let wanted = wanted.split('.').next().unwrap_or_default();
        if let Some(l) = LOCALES.iter().find(|l| l.tag.eq_ignore_ascii_case(wanted)) {
            return l.tag;
        }
        let primary = wanted.split('-').next().unwrap_or_default();
        if let Some(l) = LOCALES.iter().find(|l| {
            l.tag
                .split('-')
                .next()
                .is_some_and(|p| p.eq_ignore_ascii_case(primary))
        }) {
            return l.tag;
        }
    }
    LOCALES[0].tag
}

/// The system's preferred languages, most preferred first (read once).
pub fn system_languages() -> &'static [String] {
    static LANGUAGES: OnceLock<Vec<String>> = OnceLock::new();
    LANGUAGES.get_or_init(|| sys_locale::get_locales().collect())
}

// ---- catalogues -----------------------------------------------------------

/// A loaded translation: `namespace` → `key` → text.
pub struct Catalogue {
    entries: HashMap<String, HashMap<String, &'static str>>,
}

impl Catalogue {
    /// Parses a TOML catalogue. Values are kept for the life of the process
    /// (each language is loaded at most once).
    pub fn parse(text: &str) -> Result<Self, String> {
        let table: toml::Table = text.parse().map_err(|e| format!("{e}"))?;
        let mut entries = HashMap::new();
        for (namespace, section) in table {
            let toml::Value::Table(section) = section else {
                return Err(format!("`{namespace}` is not a section"));
            };
            let mut keys = HashMap::new();
            for (key, value) in section {
                let toml::Value::String(text) = value else {
                    return Err(format!("`{namespace}.{key}` is not a string"));
                };
                keys.insert(key, &*Box::leak(text.into_boxed_str()));
            }
            entries.insert(namespace, keys);
        }
        Ok(Self { entries })
    }

    /// The text of `namespace.key`, when the catalogue has it.
    pub fn get(&self, namespace: &str, key: &str) -> Option<&'static str> {
        self.entries.get(namespace)?.get(key).copied()
    }

    #[cfg(test)]
    fn pseudo() -> Self {
        let entries = tables()
            .iter()
            .map(|t| {
                let keys = t
                    .entries()
                    .map(|(k, en, _)| {
                        let text: &'static str = Box::leak(format!("[[{en}]]").into_boxed_str());
                        (k.to_owned(), text)
                    })
                    .collect();
                (t.namespace.to_owned(), keys)
            })
            .collect();
        Self { entries }
    }
}

fn catalogue(index: usize) -> Option<&'static Catalogue> {
    static LOADED: [OnceLock<Option<Catalogue>>; MAX_LOCALES] =
        [const { OnceLock::new() }; MAX_LOCALES];
    LOADED[index]
        .get_or_init(|| match LOCALES[index].source {
            Source::English => None,
            Source::Toml(text) => match Catalogue::parse(text) {
                Ok(c) => {
                    for problem in validate(text).unwrap_or_default() {
                        tracing::warn!(language = LOCALES[index].tag, problem, "catalogue");
                    }
                    Some(c)
                }
                Err(e) => {
                    tracing::error!(language = LOCALES[index].tag, error = %e, "bad catalogue; using English");
                    None
                }
            },
            #[cfg(test)]
            Source::Pseudo => Some(Catalogue::pseudo()),
        })
        .as_ref()
}

/// A table's strings for each language, built once on first use.
pub struct PerLocale<T: 'static> {
    slots: [OnceLock<&'static T>; MAX_LOCALES],
}

impl<T: 'static + Sync> PerLocale<T> {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self {
            slots: [const { OnceLock::new() }; MAX_LOCALES],
        }
    }

    /// The table of the active language: `english` itself, or built by
    /// `translate` from the language's catalogue.
    pub fn get(&'static self, english: &'static T, translate: fn(&Catalogue) -> T) -> &'static T {
        let index = active_index();
        let Some(catalogue) = catalogue(index) else {
            return english;
        };
        self.slots[index].get_or_init(|| Box::leak(Box::new(translate(catalogue))))
    }
}

/// A text made from catalogue strings (for a `&'static str` API), made
/// once per language.
pub struct Derived {
    slots: [OnceLock<String>; MAX_LOCALES],
}

impl Derived {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self {
            slots: [const { OnceLock::new() }; MAX_LOCALES],
        }
    }

    /// The text in the active language, made by `make` the first time.
    pub fn get(&'static self, make: impl FnOnce() -> String) -> &'static str {
        self.slots[active_index()].get_or_init(make)
    }
}

/// Declares a string table: the struct, its English instance `EN`, its
/// [`TableInfo`] `TABLE` and the accessor returning the active language's
/// strings.
///
/// ```ignore
/// string_table! {
///     namespace = "prov", struct ProvStrings, fn prov_strings;
///     /// `{vm}`
///     op_create = "Creating VM '{vm}'",
/// }
/// ```
macro_rules! string_table {
    (
        namespace = $ns:literal, struct $name:ident, fn $accessor:ident;
        $( $(#[doc = $doc:literal])* $field:ident = $en:literal, )*
    ) => {
        /// One language's strings.
        pub struct $name {
            $( $(#[doc = $doc])* pub $field: &'static str, )*
        }

        /// English.
        pub static EN: $name = $name {
            $( $field: $en, )*
        };

        /// Keys and English texts, for the catalogue.
        pub static TABLE: $crate::locale::TableInfo = $crate::locale::TableInfo {
            namespace: $ns,
            fields: &[ $( (stringify!($field), $en, &[$($doc),*]), )* ],
            pairs: &[],
        };

        /// The strings of the current language.
        pub fn $accessor() -> &'static $name {
            static LOCALIZED: $crate::locale::PerLocale<$name> = $crate::locale::PerLocale::new();
            LOCALIZED.get(&EN, |catalogue| $name {
                $( $field: catalogue.get($ns, stringify!($field)).unwrap_or(EN.$field), )*
            })
        }
    };
}
pub(crate) use string_table;

// ---- export and validation --------------------------------------------------

/// The `{name}` placeholders of a text.
pub fn placeholders(text: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end)
                if after[..end]
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_') =>
            {
                out.insert(&after[..end]);
                rest = &after[end + 1..];
            }
            _ => rest = after,
        }
    }
    out
}

/// The English catalogue in TOML: the template a translation starts from.
#[cfg(test)]
pub fn english_catalogue() -> String {
    let mut out = String::from(
        "# ReXen Center user-interface strings: the English catalogue.\n\
         #\n\
         # Generated from the string tables (`cargo test -p xenadmin` checks it;\n\
         # `XENADMIN_BLESS=1` rewrites it). Do not edit by hand: copy it to\n\
         # `<tag>.toml` to start a translation (see README.md here).\n\
         # Comments above a key name the placeholders its text takes.\n",
    );
    for table in tables() {
        out.push_str(&format!("\n[{}]\n", table.namespace));
        for (key, en, doc) in table.entries() {
            for line in doc {
                out.push_str(&format!("#{}\n", line.trim_end()));
            }
            out.push_str(&format!("{key} = {}\n", toml::Value::String(en.to_owned())));
        }
    }
    out
}

/// What is wrong with a catalogue, one line each: unknown sections and
/// keys, and texts whose placeholders differ from the English ones.
pub fn validate(text: &str) -> Result<Vec<String>, String> {
    let catalogue = Catalogue::parse(text)?;
    let tables = tables();
    let mut problems = Vec::new();
    let mut namespaces: Vec<_> = catalogue.entries.keys().collect();
    namespaces.sort();
    for namespace in namespaces {
        let Some(table) = tables.iter().find(|t| t.namespace == namespace) else {
            problems.push(format!("unknown section [{namespace}]"));
            continue;
        };
        let mut keys: Vec<_> = catalogue.entries[namespace].iter().collect();
        keys.sort();
        for (key, text) in keys {
            match table.entries().find(|(k, _, _)| k == key) {
                None => problems.push(format!("unknown key {namespace}.{key}")),
                Some((_, en, _)) if placeholders(en) != placeholders(text) => {
                    problems.push(format!(
                        "{namespace}.{key}: placeholders {:?}, English has {:?}",
                        placeholders(text),
                        placeholders(en)
                    ))
                }
                Some(_) => {}
            }
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn langs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_setting_or_the_system_picks_a_shipped_language() {
        assert_eq!(resolve("", &langs(&["fr-FR", "en-US"])), "en");
        assert_eq!(resolve("", &langs(&[])), "en");
        assert_eq!(resolve("en", &langs(&["fr"])), "en");
        // A language that does not ship falls back to English.
        assert_eq!(resolve("pt-BR", &langs(&["pt-BR"])), "en");
        // The pseudo-language stands in for a shipped translation.
        assert_eq!(resolve("", &langs(&["en_XA.UTF-8"])), "en-XA");
        assert_eq!(resolve("EN-xa", &langs(&[])), "en-XA");
        assert_eq!(resolve("", &langs(&["en-GB"])), "en");
    }

    #[test]
    fn namespaces_and_keys_are_unique() {
        let mut seen = BTreeSet::new();
        for t in tables() {
            assert!(
                seen.insert(t.namespace),
                "duplicate namespace {}",
                t.namespace
            );
            let mut keys = BTreeSet::new();
            for (k, _, _) in t.entries() {
                assert!(keys.insert(k), "duplicate key {}.{k}", t.namespace);
            }
        }
    }

    #[test]
    fn placeholders_are_found() {
        let p = placeholders("Moving {vm} to '{sr}' ({index} of {count}) {not a placeholder}");
        assert_eq!(
            p.into_iter().collect::<Vec<_>>(),
            ["count", "index", "sr", "vm"]
        );
        assert!(placeholders("{}").contains(""));
    }

    /// `locales/en.toml` is the tables' English, so translators can copy it.
    #[test]
    fn the_english_catalogue_file_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales/en.toml");
        let current = english_catalogue();
        if std::env::var_os("XENADMIN_BLESS").is_some() {
            std::fs::write(&path, &current).unwrap();
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            on_disk == current,
            "locales/en.toml is out of date; run `XENADMIN_BLESS=1 cargo test -p xenadmin english_catalogue`"
        );
        assert_eq!(validate(&on_disk).unwrap(), Vec::<String>::new());
        // Parsed back, it gives every English text.
        let parsed = Catalogue::parse(&on_disk).unwrap();
        for t in tables() {
            for (k, en, _) in t.entries() {
                assert_eq!(parsed.get(t.namespace, k), Some(en));
            }
        }
    }

    /// Every catalogue that ships is complete enough to load and has the
    /// English placeholders.
    #[test]
    fn shipped_catalogues_are_valid() {
        for l in LOCALES {
            if let Source::Toml(text) = l.source {
                assert_eq!(validate(text).unwrap(), Vec::<String>::new(), "{}", l.tag);
            }
        }
    }

    #[test]
    fn validation_reports_unknown_keys_and_placeholder_changes() {
        let problems = validate(
            "[main]\nmenu_file = \"Arquivo\"\nno_such_key = \"x\"\n\
             status_connecting = \"Conectando…\"\n[nowhere]\nx = \"y\"\n",
        )
        .unwrap();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("no_such_key"));
        assert!(problems[1].contains("status_connecting"));
        assert!(problems[2].contains("[nowhere]"));
        assert!(validate("[main]\nmenu_file = 3\n").is_err());
    }

    /// A translation replaces the keys it has; the rest stay English.
    #[test]
    fn a_catalogue_overrides_by_key_with_english_fallback() {
        let c = Catalogue::parse("[main]\nmenu_file = \"Arquivo\"\n").unwrap();
        assert_eq!(c.get("main", "menu_file"), Some("Arquivo"));
        assert_eq!(c.get("main", "menu_view"), None);
        assert_eq!(c.get("ops", "menu_file"), None);
    }

    #[test]
    fn every_table_follows_the_active_language() {
        set_active_for_this_thread(Some("en-XA"));
        assert_eq!(crate::i18n::strings().menu_file, "[[File]]");
        assert_eq!(crate::prov_strings::prov_strings().new_vm, "[[New VM...]]");
        assert_eq!(crate::sr_strings::sr_strings().menu_storage, "[[Storage]]");
        assert_eq!(active().tag, "en-XA");
        assert_eq!(
            crate::i18n::friendly_error("HOST_DISABLED-SHORT", &[]).as_deref(),
            Some("[[Disabled]]")
        );
        assert_eq!(
            crate::provision_ui::install_tools_label(),
            "[[Install {tools}...]]"
                .replace("{tools}", crate::prov_strings::prov_strings().tools_name)
                .as_str()
        );
        set_active_for_this_thread(None);
        assert_eq!(
            crate::i18n::friendly_error("HOST_DISABLED-SHORT", &[]).as_deref(),
            Some("Disabled")
        );
        assert_eq!(crate::i18n::strings().menu_file, "File");
        assert_eq!(active().tag, "en");
    }
}
