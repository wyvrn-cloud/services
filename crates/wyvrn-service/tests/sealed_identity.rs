//! Keys sealed at rest (identity.rs's `seal_with`): a process of its own, since the
//! key is the process's.

use wyvrn_service::db::Location;
use wyvrn_service::identity;

#[test]
fn keys_are_sealed_at_rest_read_back_and_those_found_unsealed_are_sealed_only_when_the_operator_says() {
    let dir = std::env::temp_dir().join(format!("sealed-identity-{}", std::process::id()));
    let community = dir.join("den");
    std::fs::create_dir_all(&community).unwrap();
    let location = Location::Sqlite(community.join("community.sqlite"));
    // Kept before there was a key: as the file holds them.
    let path = community.join("identity.json");
    let made = identity::load_or_generate(&location, &path, "community").unwrap();
    let plain = std::fs::read_to_string(&path).unwrap();
    assert!(!plain.contains("wyvrn-sealed-identity"));

    identity::seal_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=").unwrap();
    // Found unsealed once there is a key: refused, as someone who can write the
    // file could have put keys of their own there (WYV-32)...
    assert!(identity::load(&location, &path, "community").is_err());
    // ...unless the operator says these are the ones to seal, for the start that does.
    std::env::set_var("WYVRN_SEAL_ADOPT_PLAIN", "true");
    // Read with the key: the same keys, and sealed from then on.
    let read = identity::load(&location, &path, "community").unwrap().unwrap();
    assert_eq!(read.to_json().unwrap(), made.to_json().unwrap());
    let sealed = std::fs::read_to_string(&path).unwrap();
    assert!(sealed.contains("wyvrn-sealed-identity/1"));
    std::env::remove_var("WYVRN_SEAL_ADOPT_PLAIN");
    assert!(!sealed.contains(&made.to_json().unwrap()[..40]), "nothing of the keys readable");
    assert_eq!(identity::load(&location, &path, "community").unwrap().unwrap().to_json().unwrap(), made.to_json().unwrap());

    // Sealed for one community, they don't open as another's (moved files).
    let other = dir.join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(&path, other.join("identity.json")).unwrap();
    assert!(identity::load(&Location::Sqlite(other.join("community.sqlite")), &other.join("identity.json"), "community").is_err());

    // A new one is made sealed, and a second key is refused.
    let fresh = dir.join("fresh");
    identity::load_or_generate(&Location::Sqlite(fresh.join("community.sqlite")), &fresh.join("identity.json"), "community").unwrap();
    assert!(std::fs::read_to_string(fresh.join("identity.json")).unwrap().contains("wyvrn-sealed-identity/1"));
    assert!(identity::seal_with("ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8=").is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
