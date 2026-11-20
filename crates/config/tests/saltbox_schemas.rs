//! Saltbox's own schemas, pinned from the public repository
//! (fixtures/saltbox-schemas/PROVENANCE.md), load strictly (config spec,
//! decision 5; Task 1's inventory).

use std::path::Path;

use sb_config::schema::{self, Additional, Kind, Scalar};

fn pinned(name: &str) -> schema::Rules {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/saltbox-schemas")
        .join(name);
    schema::load(&path, name).unwrap_or_else(|err| panic!("{err}"))
}

#[test]
fn every_pinned_schema_loads() {
    for name in [
        "accounts.schema.yml",
        "adv_settings.schema.yml",
        "backup_config.schema.yml",
        "hetzner_vlan.schema.yml",
        "motd.schema.yml",
        "providers.schema.yml",
        "settings.schema.yml",
    ] {
        assert!(!pinned(name).is_empty(), "{name}");
    }
}

#[test]
fn the_pinned_schemas_keep_what_validation_needs() {
    let accounts = pinned("accounts.schema.yml");
    let user = accounts["user"].properties.as_ref().expect("user");
    assert!(matches!(user["email"].not_equals, Some(Scalar::Str(_))));
    let motd = pinned("motd.schema.yml");
    let names = motd["systemd"].properties.as_ref().expect("systemd")["display_names"]
        .additional_properties
        .as_ref()
        .expect("additionalProperties");
    assert!(matches!(names, Additional::Rule(rule) if rule.kind == Some(Kind::String)));
}
