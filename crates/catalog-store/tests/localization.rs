//! Localization is a **dimension**, not a naming convention.
//!
//! # Why this exists at all
//!
//! **24 of adiona's 58 tables are `_translate` / `_content`.** Dropping localization —
//! which the spec's assumption 8 originally did — would make adiona a *false* worked
//! example: nearly half its schema would be inexpressible, and the catalog would not be
//! a generalization of the model it claims to generalize.
//!
//! The rejected alternative was encoding the language into the attribute name
//! (`category_name@de`). A probe confirmed it "works", and it is wrong: the language
//! becomes unqueryable, `attributes()` reports one entry per language as though they
//! were different attributes, and nothing can ask which languages exist or fall back to
//! a default.
//!
//! # The RED
//!
//! With the fold keyed on `name` alone, writing `category_name` in `en` and then in `de`
//! loses the first — a successful-looking write that silently overwrites a translation:
//!
//! ```text
//! translations of category_name: 1, expected 2
//! ```

use catalog_model::{Attribute, AttributeValue, Entity};
use catalog_store::{CatalogStore, StoreConfig};

fn ent(path: &str) -> Entity {
    Entity {
        resource_type: "category".into(),
        path: path.into(),
        version: 1,
        entity_id: 10,
        content: None,
        content_sha256: "0".repeat(64),
        archived_at: None,
    }
}
fn txt(s: &str) -> AttributeValue {
    AttributeValue::Text(s.into())
}

/// adiona's `category_content` shape: one row per (category, lang).
fn seed(store: &mut CatalogStore) {
    store.register(ent("categories/10")).expect("r");
    // The language-NEUTRAL base value — adiona's `categories.category_name`.
    store
        .set_attribute(
            "categories/10",
            Attribute::new(10, "category_name", txt("Beach")),
        )
        .expect("a");
    // Translations — adiona's `category_content` rows.
    for (lang, v) in [("en", "Beach"), ("de", "Strand"), ("ka", "ზღვისპირა")] {
        store
            .set_attribute(
                "categories/10",
                Attribute::localized(10, "category_name", txt(v), lang),
            )
            .expect("a");
    }
}

#[test]
fn every_translation_of_one_attribute_survives() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed(&mut s);

    let all = s.attributes_all_langs("categories/10").expect("all");
    let names: Vec<_> = all.keys().cloned().collect();
    println!("folded entries: {names:?}");

    // 1 neutral + 3 translations of the SAME attribute name.
    let translations: Vec<_> = all
        .iter()
        .filter(|((n, l), _)| n == "category_name" && l.is_some())
        .collect();
    println!(
        "translations of category_name: {}, expected 3",
        translations.len()
    );
    assert_eq!(
        translations.len(),
        3,
        "a translation overwrote another — the fold is keyed on name alone"
    );

    // ⚠ Set equality on the (name, lang) keys, not a count.
    let mut keys: Vec<(String, Option<String>)> = all.keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            ("category_name".to_string(), None),
            ("category_name".to_string(), Some("de".into())),
            ("category_name".to_string(), Some("en".into())),
            ("category_name".to_string(), Some("ka".into())),
        ]
    );

    // And the VALUES are the right ones, not just the right number of them.
    assert_eq!(
        all[&("category_name".to_string(), Some("de".into()))].value,
        txt("Strand")
    );
    assert_eq!(
        all[&("category_name".to_string(), Some("ka".into()))].value,
        txt("ზღვისპირა")
    );
    assert_eq!(
        all[&("category_name".to_string(), None)].value,
        txt("Beach")
    );
}

/// `None` is not `Some("en")`. A playbook's `uses_tool.postgres` has no language;
/// conflating them would make every noetl attribute pretend to be English.
#[test]
fn language_neutral_is_distinct_from_english() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed(&mut s);

    // The neutral read — what noetl callers use, and it must NOT include translations.
    let neutral = s.attributes("categories/10").expect("neutral");
    println!(
        "neutral attributes: {:?}",
        neutral.keys().collect::<Vec<_>>()
    );
    assert_eq!(neutral.len(), 1, "the neutral read leaked translations");
    assert!(neutral.contains_key("category_name"));
    assert_eq!(neutral["category_name"].lang, None);
}

/// The query localization exists for: read in a language, falling back when a
/// translation is absent.
#[test]
fn a_localized_read_falls_back_when_a_translation_is_missing() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed(&mut s);
    // An attribute with NO translation at all.
    s.set_attribute(
        "categories/10",
        Attribute::new(10, "default_lang_code", txt("en")),
    )
    .expect("a");

    let de = s.attributes_in("categories/10", "de").expect("de");
    println!(
        "de read: {:?}",
        de.iter().map(|(k, v)| (k, &v.value)).collect::<Vec<_>>()
    );
    assert_eq!(
        de["category_name"].value,
        txt("Strand"),
        "no German translation used"
    );
    assert_eq!(
        de["default_lang_code"].value,
        txt("en"),
        "an untranslated attribute must fall back to the neutral value, not vanish"
    );

    // A language with no translations at all falls back entirely.
    let fr = s.attributes_in("categories/10", "fr").expect("fr");
    assert_eq!(
        fr["category_name"].value,
        txt("Beach"),
        "an absent language must fall back to the neutral value"
    );

    // ⚠ Case-insensitive: `lang_code` arrives as en/EN/En across adiona's columns, and
    // two spellings of one language would be two translations.
    assert_eq!(
        s.attributes_in("categories/10", "DE").expect("DE")["category_name"].value,
        txt("Strand")
    );
}

/// Which languages exist for a path — unanswerable if the language is in the name.
#[test]
fn the_available_languages_are_queryable() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed(&mut s);
    let langs = s.languages_of("categories/10").expect("langs");
    println!("languages: {langs:?}");
    assert_eq!(
        langs,
        vec!["de".to_string(), "en".to_string(), "ka".to_string()]
    );
}

/// Unsetting one translation must leave the others, and leave the neutral value.
#[test]
fn unsetting_one_translation_leaves_the_rest() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed(&mut s);

    s.unset_localized_attribute("categories/10", "category_name", "de")
        .expect("unset de");

    let langs = s.languages_of("categories/10").expect("langs");
    println!("languages after unsetting de: {langs:?}");
    assert_eq!(langs, vec!["en".to_string(), "ka".to_string()]);

    // The neutral value is untouched — unsetting a translation is not unsetting the
    // attribute.
    let neutral = s.attributes("categories/10").expect("n");
    assert_eq!(neutral["category_name"].value, txt("Beach"));

    // And a de read now falls back.
    assert_eq!(
        s.attributes_in("categories/10", "de").expect("de")["category_name"].value,
        txt("Beach")
    );
}

/// Translations survive a reopen, like every other fold here.
#[test]
fn translations_survive_a_reopen() {
    let dir = tempfile::tempdir().expect("td");
    {
        let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
        seed(&mut s);
    }
    let s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("reopen");
    assert_eq!(s.languages_of("categories/10").expect("l").len(), 3);
    assert_eq!(
        s.attributes_in("categories/10", "ka").expect("ka")["category_name"].value,
        txt("ზღვისპირა")
    );
}
