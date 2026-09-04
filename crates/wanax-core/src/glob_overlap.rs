use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

fn compile_globs(patterns: &[String]) -> Result<GlobSet, ()> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let g = GlobBuilder::new(p)
            .literal_separator(false)
            .build()
            .map_err(|_| ())?;
        b.add(g);
    }
    b.build().map_err(|_| ())
}

/// True when two allowed-glob sets could match the same repo-relative path.
pub fn peer_glob_sets_overlap(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    for pa in a {
        for pb in b {
            if pa == pb {
                return true;
            }
        }
    }
    let (Ok(set_a), Ok(set_b)) = (compile_globs(a), compile_globs(b)) else {
        return true;
    };
    for probe in probe_paths(a, b) {
        if set_a.is_match(&probe) && set_b.is_match(&probe) {
            return true;
        }
    }
    false
}

/// Returns indices of the first overlapping peer pair, if any.
pub fn find_peer_overlap(sets: &[Vec<String>]) -> Option<(usize, usize)> {
    for i in 0..sets.len() {
        for j in (i + 1)..sets.len() {
            if peer_glob_sets_overlap(&sets[i], &sets[j]) {
                return Some((i, j));
            }
        }
    }
    None
}

fn probe_paths(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in a.iter().chain(b.iter()) {
        out.extend(pattern_probes(p));
    }
    out.sort();
    out.dedup();
    out
}

fn pattern_probes(pattern: &str) -> Vec<String> {
    if !pattern.contains('*') {
        return vec![pattern.to_string()];
    }
    let prefix = pattern.split('*').next().unwrap_or("").trim_end_matches('/');
    if prefix.is_empty() {
        return vec!["file.rs".into(), "src/file.rs".into(), "a/b/c.rs".into()];
    }
    vec![
        format!("{prefix}"),
        format!("{prefix}/file.rs"),
        format!("{prefix}/nested/x.rs"),
    ]
}

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
}
