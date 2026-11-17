//! Keeps the Rust model and `src/lib/annotate.ts` in step, and keeps old documents readable.
//!
//! Both sides read the same fixture, which has an object of every kind, and both must write it
//! back unchanged; `src/lib/annotate.test.ts` is the other half. A field renamed on one side only
//! makes that side's test fail, before a saved document silently loses the field.
//!
//! `fixtures/annotate/versions/` holds one document for every older format version. Each must
//! load through the migration with every object it had, in the same order, with every field it
//! wrote unchanged, because a saved capture whose drawings vanish on the next launch cannot be
//! repaired afterwards.

use std::path::PathBuf;

use glimpsfy_annotate::{migrate, version_of, Document, ObjectKind, CURRENT_VERSION};
use serde_json::Value;

const FIXTURE: &str = include_str!("../../../fixtures/annotate/document.json");

/// JSON does not tell `2` from `2.0`, and neither does TypeScript, so numbers are compared by
/// value and the comparison stays about names and structure.
fn normalised(value: Value) -> Value {
    match value {
        Value::Number(n) => n.as_f64().map(Value::from).unwrap_or(Value::Number(n)),
        Value::Array(items) => Value::Array(items.into_iter().map(normalised).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, normalised(v))).collect()),
        other => other,
    }
}

#[test]
fn the_shared_fixture_round_trips_through_the_rust_model_unchanged() {
    let original: Value = serde_json::from_str(FIXTURE).expect("the fixture is JSON");
    assert_eq!(
        version_of(&original).expect("the fixture has a version"),
        CURRENT_VERSION,
        "the shared fixture is written in the current version"
    );
    let document = Document::from_json(FIXTURE).expect("the fixture is a valid document");
    let written = serde_json::to_value(&document).expect("a document serialises");

    assert_eq!(normalised(written), normalised(original));

    for kind in ObjectKind::ALL {
        assert!(document.objects.iter().any(|object| object.kind() == kind), "the fixture has no {kind:?} object");
    }
}

#[test]
fn every_older_version_loads_through_the_migration_without_losing_an_object() {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/annotate/versions");
    let mut seen = 0;
    for entry in std::fs::read_dir(&folder).expect("the versions folder exists") {
        let path = entry.expect("a directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        seen += 1;
        let name = path.display().to_string();
        let text = std::fs::read_to_string(&path).expect("the fixture is readable");
        let original: Value = serde_json::from_str(&text).expect("the fixture is JSON");
        assert!(version_of(&original).expect("a version") < CURRENT_VERSION, "{name} is not an older version");

        let document = Document::from_json(&text).unwrap_or_else(|e| panic!("{name} does not load: {e}"));
        assert_eq!(document.version, CURRENT_VERSION, "{name} was not brought up to the current version");

        let before = original["objects"].as_array().expect("an object list");
        assert_eq!(document.objects.len(), before.len(), "{name} lost objects");
        let after = serde_json::to_value(&document).expect("a document serialises");
        for (old, new) in before.iter().zip(after["objects"].as_array().expect("an object list")) {
            // Every field the old version wrote comes back with the same value.
            for (key, value) in old.as_object().expect("an object") {
                assert_eq!(
                    normalised(new[key].clone()),
                    normalised(value.clone()),
                    "{name}: {key} of {} changed",
                    old["id"]
                );
            }
        }
        // The image and the scale survive as well.
        assert_eq!(
            normalised(after["image"].clone()),
            normalised(original["image"].clone()),
            "{name}: the image changed"
        );
        assert_eq!(
            normalised(after["scale"].clone()),
            normalised(original["scale"].clone()),
            "{name}: the scale changed"
        );

        // Migrating twice changes nothing.
        let once = migrate(original.clone()).expect("the migration runs");
        assert_eq!(
            migrate(once.clone()).expect("the migration runs again"),
            once,
            "{name}: the migration is not idempotent"
        );
    }
    assert!(seen > 0, "no fixture of an older version was found in {}", folder.display());
}
