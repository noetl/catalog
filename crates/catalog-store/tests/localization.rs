//! The `lang` dimension on an attribute — **a store primitive with no confirmed internal
//! consumer**, and the architecture now says why.
//!
//! # ⚠⚠ Localization belongs to the BUSINESS catalog, not this one
//!
//! This dimension was added while the catalog's scope still treated adiona's schema as a
//! target, citing that **24 of its 58 tables are `_translate` / `_content`**. With the
//! two-catalog line drawn, those tables are clearly **business-catalog** concerns:
//!
//! * `noetl/travel`'s `adiona/playbooks/catalog_list.yaml` reads
//!   `adiona.item_content … WHERE c.lang_code IN (COALESCE(j->>'lang','en'), …)` — the
//!   localization lives in **external Postgres**, reached by a playbook step under that
//!   playbook's policy block.
//! * The **internal** catalog holds the *playbook object that performs that read*, plus its
//!   `uses_tool.postgres` and `uses_credential.adiona_actor` metadata. It does not hold the
//!   item, the category, or their translations.
//!
//! So: **no internal noetl object type has a localization requirement that has been
//! demonstrated.** The dimension is kept because it is already built, tested and inert
//! (`lang` defaults to `None`; the neutral read excludes translations, so every noetl path
//! is byte-identical), and is **not built on further**.
//!
//! ⚠ The fixture below therefore uses `memory` — a real noetl internal object type ("AI
//! memory, knowledge, or coordination artifact") that could *plausibly* carry
//! human-readable text — rather than adiona's `category`, which an earlier draft used. That
//! earlier fixture modelled a **business entity as an internal object**, which is the exact
//! conflation the two-catalog section exists to prevent. A fixture teaches what belongs
//! here, so it has to be right even though the store cannot tell the difference.
//!
//! Using `memory` is **not** a claim that noetl memories are localized. It is a type that
//! is at least *internal*.
//!
//! # The RED
//!
//! With the fold keyed on `name` alone, writing one attribute in two languages loses the
//! first — a successful-looking write that silently overwrites a translation:
//!
//! ```text
//! folded entries: [("display_name", None)]
//! translations of display_name: 0, expected 3
//! ```

use catalog_model::{Attribute, AttributeValue, Entity};
use catalog_store::{CatalogStore, StoreConfig};

fn ent(path: &str) -> Entity {
    Entity {
        resource_type: "memory".into(),
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
    store.register(ent("memory/notes/welcome")).expect("r");
    // The language-NEUTRAL base value — adiona's `categories.display_name`.
    store
        .set_attribute(
            "memory/notes/welcome",
            Attribute::new(10, "display_name", txt("Beach")),
        )
        .expect("a");
    // Translations — adiona's `category_content` rows.
    for (lang, v) in [("en", "Beach"), ("de", "Strand"), ("ka", "ზღვისპირა")] {
        store
            .set_attribute(
                "memory/notes/welcome",
                Attribute::localized(10, "display_name", txt(v), lang),
            )
            .expect("a");
    }
}

#[test]
fn every_translation_of_one_attribute_survives() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed(&mut s);

    let all = s.attributes_all_langs("memory/notes/welcome").expect("all");
    let names: Vec<_> = all.keys().cloned().collect();
    println!("folded entries: {names:?}");

    // 1 neutral + 3 translations of the SAME attribute name.
    let translations: Vec<_> = all
        .iter()
        .filter(|((n, l), _)| n == "display_name" && l.is_some())
        .collect();
    println!(
        "translations of display_name: {}, expected 3",
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
            ("display_name".to_string(), None),
            ("display_name".to_string(), Some("de".into())),
            ("display_name".to_string(), Some("en".into())),
            ("display_name".to_string(), Some("ka".into())),
        ]
    );

    // And the VALUES are the right ones, not just the right number of them.
    assert_eq!(
        all[&("display_name".to_string(), Some("de".into()))].value,
        txt("Strand")
    );
    assert_eq!(
        all[&("display_name".to_string(), Some("ka".into()))].value,
        txt("ზღვისპირა")
    );
    assert_eq!(all[&("display_name".to_string(), None)].value, txt("Beach"));
}

/// `None` is not `Some("en")`. A playbook's `uses_tool.postgres` has no language;
/// conflating them would make every noetl attribute pretend to be English.
#[test]
fn language_neutral_is_distinct_from_english() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed(&mut s);

    // The neutral read — what noetl callers use, and it must NOT include translations.
    let neutral = s.attributes("memory/notes/welcome").expect("neutral");
    println!(
        "neutral attributes: {:?}",
        neutral.keys().collect::<Vec<_>>()
    );
    assert_eq!(neutral.len(), 1, "the neutral read leaked translations");
    assert!(neutral.contains_key("display_name"));
    assert_eq!(neutral["display_name"].lang, None);
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
        "memory/notes/welcome",
        Attribute::new(10, "default_lang_code", txt("en")),
    )
    .expect("a");

    let de = s.attributes_in("memory/notes/welcome", "de").expect("de");
    println!(
        "de read: {:?}",
        de.iter().map(|(k, v)| (k, &v.value)).collect::<Vec<_>>()
    );
    assert_eq!(
        de["display_name"].value,
        txt("Strand"),
        "no German translation used"
    );
    assert_eq!(
        de["default_lang_code"].value,
        txt("en"),
        "an untranslated attribute must fall back to the neutral value, not vanish"
    );

    // A language with no translations at all falls back entirely.
    let fr = s.attributes_in("memory/notes/welcome", "fr").expect("fr");
    assert_eq!(
        fr["display_name"].value,
        txt("Beach"),
        "an absent language must fall back to the neutral value"
    );

    // ⚠ Case-insensitive: `lang_code` arrives as en/EN/En across adiona's columns, and
    // two spellings of one language would be two translations.
    assert_eq!(
        s.attributes_in("memory/notes/welcome", "DE").expect("DE")["display_name"].value,
        txt("Strand")
    );
}

/// Which languages exist for a path — unanswerable if the language is in the name.
#[test]
fn the_available_languages_are_queryable() {
    let dir = tempfile::tempdir().expect("td");
    let mut s = CatalogStore::open(&StoreConfig::new(dir.path())).expect("open");
    seed(&mut s);
    let langs = s.languages_of("memory/notes/welcome").expect("langs");
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

    s.unset_localized_attribute("memory/notes/welcome", "display_name", "de")
        .expect("unset de");

    let langs = s.languages_of("memory/notes/welcome").expect("langs");
    println!("languages after unsetting de: {langs:?}");
    assert_eq!(langs, vec!["en".to_string(), "ka".to_string()]);

    // The neutral value is untouched — unsetting a translation is not unsetting the
    // attribute.
    let neutral = s.attributes("memory/notes/welcome").expect("n");
    assert_eq!(neutral["display_name"].value, txt("Beach"));

    // And a de read now falls back.
    assert_eq!(
        s.attributes_in("memory/notes/welcome", "de").expect("de")["display_name"].value,
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
    assert_eq!(s.languages_of("memory/notes/welcome").expect("l").len(), 3);
    assert_eq!(
        s.attributes_in("memory/notes/welcome", "ka").expect("ka")["display_name"].value,
        txt("ზღვისპირა")
    );
}
