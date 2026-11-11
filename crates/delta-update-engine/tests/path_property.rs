use delta_update_engine::path::validate_rel_path;
use proptest::prelude::*;

proptest! {
    #[test]
    fn accepts_well_formed_paths(segments in prop::collection::vec("f_[A-Za-z0-9_-]{1,16}", 1..6)) {
        let path = segments.join("/");
        prop_assert!(validate_rel_path(&path).is_ok());
    }

    #[test]
    fn rejects_absolute_paths(suffix in "f_[A-Za-z0-9_-]{1,8}") {
        let path = format!("/{suffix}");
        prop_assert!(validate_rel_path(&path).is_err());
    }

    #[test]
    fn rejects_traversal_segments(
        prefix in "f_[A-Za-z0-9_-]{1,8}",
        suffix in "f_[A-Za-z0-9_-]{1,8}",
    ) {
        let path = format!("{prefix}/../{suffix}");
        prop_assert!(validate_rel_path(&path).is_err());
    }

    #[test]
    fn rejects_backslash_separators(suffix in "f_[A-Za-z0-9_-]{1,8}") {
        let path = format!("dir\\{suffix}");
        prop_assert!(validate_rel_path(&path).is_err());
    }
}
