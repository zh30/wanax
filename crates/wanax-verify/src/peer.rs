pub use wanax_core::{find_peer_overlap, peer_glob_sets_overlap};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_globs_overlap() {
        assert!(peer_glob_sets_overlap(
            &["src/a/**".into()],
            &["src/a/**".into()],
        ));
    }

    #[test]
    fn parent_child_globs_overlap() {
        assert!(peer_glob_sets_overlap(
            &["src/**".into()],
            &["src/a/**".into()]
        ));
    }

    #[test]
    fn disjoint_modules_do_not_overlap() {
        assert!(!peer_glob_sets_overlap(
            &["src/a.rs".into()],
            &["src/b.rs".into()],
        ));
        assert!(!peer_glob_sets_overlap(
            &["src/a/**".into()],
            &["src/b/**".into()],
        ));
    }

    #[test]
    fn find_overlap_returns_indices() {
        let sets = vec![
            vec!["src/a/**".into()],
            vec!["src/b/**".into()],
            vec!["src/a/nested/**".into()],
        ];
        assert_eq!(find_peer_overlap(&sets), Some((0, 2)));
    }
}
