use crate::error::{ErrorCode, WanaxError};
use std::collections::{HashMap, HashSet, VecDeque};

/// Topologically sort work-unit local ids. `nodes` is (id, depends_on).
pub fn topo_sort(nodes: &[(String, Vec<String>)]) -> Result<Vec<String>, WanaxError> {
    let ids: HashSet<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();
    if ids.len() != nodes.len() {
        return Err(WanaxError::from_code(ErrorCode::DagCycle));
    }
    let mut indeg: HashMap<&str, usize> = HashMap::new();
    let mut kids: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, _) in nodes {
        indeg.entry(id.as_str()).or_insert(0);
        kids.entry(id.as_str()).or_default();
    }
    for (id, deps) in nodes {
        let mut seen = HashSet::new();
        for dep in deps {
            if !ids.contains(dep.as_str()) {
                return Err(WanaxError::with_detail(
                    ErrorCode::CommanderSchema,
                    format!("unknown DAG dependency {dep}"),
                ));
            }
            if !seen.insert(dep.as_str()) {
                continue;
            }
            *indeg.entry(id.as_str()).or_insert(0) += 1;
            kids.entry(dep.as_str()).or_default().push(id.as_str());
        }
    }
    let mut q: VecDeque<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();
    q.make_contiguous().sort();
    let mut out = Vec::new();
    while let Some(id) = q.pop_front() {
        out.push(id.to_string());
        let mut nxt = kids.get(id).cloned().unwrap_or_default();
        nxt.sort();
        for child in nxt {
            if let Some(d) = indeg.get_mut(child) {
                *d = d.saturating_sub(1);
                if *d == 0 {
                    q.push_back(child);
                }
            }
        }
    }
    if out.len() != nodes.len() {
        return Err(WanaxError::from_code(ErrorCode::DagCycle));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_linear_chain() {
        let nodes = vec![
            ("u2".into(), vec!["u1".into()]),
            ("u1".into(), vec![]),
            ("u3".into(), vec!["u2".into()]),
        ];
        assert_eq!(topo_sort(&nodes).unwrap(), vec!["u1", "u2", "u3"]);
    }

    #[test]
    fn rejects_cycle() {
        let nodes = vec![
            ("a".into(), vec!["b".into()]),
            ("b".into(), vec!["a".into()]),
        ];
        let err = topo_sort(&nodes).unwrap_err();
        assert_eq!(err.code, ErrorCode::DagCycle);
    }

    #[test]
    fn rejects_unknown_dep() {
        let nodes = vec![("a".into(), vec!["missing".into()])];
        let err = topo_sort(&nodes).unwrap_err();
        assert_eq!(err.code, ErrorCode::CommanderSchema);
    }
}
