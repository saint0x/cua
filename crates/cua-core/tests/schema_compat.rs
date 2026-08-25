use cua_core::schema_bundle;

#[test]
fn schema_bundle_matches_checked_in_fixture() {
    let current = serde_json::to_value(schema_bundle()).expect("serialize current schema bundle");
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/schema-bundle.json"))
            .expect("parse checked-in schema fixture");
    assert_eq!(current, fixture);
}
