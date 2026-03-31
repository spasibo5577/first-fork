//! Service dependency graph with cycle detection and root-cause analysis.
//!
//! DAG where edges mean "depends on": `AdGuard` → `Unbound`.
//! Built once at startup from config. Immutable after construction.

use crate::model::ServiceId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// An immutable directed acyclic dependency graph.
#[derive(Debug)]
pub struct DepGraph {
    /// service → services it depends on (parents).
    parents: BTreeMap<ServiceId, Vec<ServiceId>>,
    all_ids: Vec<ServiceId>,
    topo_order: Vec<ServiceId>,
}

impl DepGraph {
    /// Builds the dependency graph from service specs.
    ///
    /// # Errors
    /// Returns an error if the graph contains a cycle, self-dependency,
    /// or references an undefined service.
    pub fn build(services: &[crate::config::ServiceEntry]) -> Result<Self, GraphError> {
        let mut parents: BTreeMap<ServiceId, Vec<ServiceId>> = BTreeMap::new();
        let mut all_ids = Vec::with_capacity(services.len());

        let known: BTreeSet<ServiceId> = services.iter().map(|s| s.id.clone()).collect();

        for svc in services {
            all_ids.push(svc.id.clone());
            parents.entry(svc.id.clone()).or_default();
            for dep in &svc.depends_on {
                if !known.contains(dep) {
                    return Err(GraphError::UnknownDependency {
                        service: svc.id.clone(),
                        dependency: dep.clone(),
                    });
                }
                if dep == &svc.id {
                    return Err(GraphError::SelfDependency {
                        service: svc.id.clone(),
                    });
                }
                parents.entry(svc.id.clone()).or_default().push(dep.clone());
            }
        }

        let topo_order = topological_sort(&all_ids, &parents)?;

        Ok(Self {
            parents,
            all_ids,
            topo_order,
        })
    }

    /// Services in dependency order (dependencies first).
    #[must_use]
    pub fn topological_order(&self) -> &[ServiceId] {
        &self.topo_order
    }

    /// Direct dependencies (parents) of a service.
    #[must_use]
    pub fn dependencies_of(&self, id: &ServiceId) -> &[ServiceId] {
        self.parents.get(id).map_or(&[], Vec::as_slice)
    }

    /// Given unhealthy services, separates root causes from blocked dependents.
    ///
    /// A root cause is an unhealthy service with no unhealthy parents.
    /// Returns `(root_causes, blocked → root_cause)`.
    #[must_use]
    pub fn classify_failures(
        &self,
        unhealthy: &BTreeSet<ServiceId>,
    ) -> (Vec<ServiceId>, BTreeMap<ServiceId, ServiceId>) {
        let mut root_causes = Vec::new();
        let mut blocked = BTreeMap::new();

        for id in unhealthy {
            let has_unhealthy_parent = self
                .dependencies_of(id)
                .iter()
                .any(|dep| unhealthy.contains(dep));

            if has_unhealthy_parent {
                if let Some(root) = self.find_root_cause(id, unhealthy) {
                    blocked.insert(id.clone(), root);
                }
            } else {
                root_causes.push(id.clone());
            }
        }

        (root_causes, blocked)
    }

    /// Walks up dependency chain to find the ultimate root cause.
    fn find_root_cause(
        &self,
        start: &ServiceId,
        unhealthy: &BTreeSet<ServiceId>,
    ) -> Option<ServiceId> {
        let mut current = start.clone();
        let mut visited = BTreeSet::new();

        loop {
            if !visited.insert(current.clone()) {
                return Some(current); // cycle guard (shouldn't happen in DAG)
            }

            let unhealthy_parents: Vec<&ServiceId> = self
                .dependencies_of(&current)
                .iter()
                .filter(|dep| unhealthy.contains(*dep))
                .collect();

            if unhealthy_parents.is_empty() {
                return Some(current);
            }

            current = unhealthy_parents[0].clone();
        }
    }

    /// All service IDs in the graph.
    #[must_use]
    pub fn all_services(&self) -> &[ServiceId] {
        &self.all_ids
    }
}

/// Kahn's algorithm for topological sort. Detects cycles.
fn topological_sort(
    all_ids: &[ServiceId],
    parents: &BTreeMap<ServiceId, Vec<ServiceId>>,
) -> Result<Vec<ServiceId>, GraphError> {
    // in_degree[X] = number of dependencies X has = parents[X].len()
    let mut in_degree: BTreeMap<ServiceId, usize> = BTreeMap::new();
    for id in all_ids {
        in_degree.insert(id.clone(), parents.get(id).map_or(0, Vec::len));
    }

    // Seed queue with nodes that have no dependencies.
    let mut queue: VecDeque<ServiceId> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(id, _)| id.clone())
        .collect();

    // Reverse map: for each node, who depends on it?
    let mut dependents: BTreeMap<ServiceId, Vec<ServiceId>> = BTreeMap::new();
    for id in all_ids {
        dependents.entry(id.clone()).or_default();
    }
    for (id, deps) in parents {
        for dep in deps {
            dependents.entry(dep.clone()).or_default().push(id.clone());
        }
    }

    let mut order = Vec::with_capacity(all_ids.len());

    while let Some(node) = queue.pop_front() {
        order.push(node.clone());

        if let Some(children) = dependents.get(&node) {
            for child in children {
                if let Some(deg) = in_degree.get_mut(child) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }
    }

    if order.len() != all_ids.len() {
        let in_cycle: Vec<ServiceId> = all_ids
            .iter()
            .filter(|id| !order.contains(id))
            .cloned()
            .collect();
        return Err(GraphError::Cycle { involved: in_cycle });
    }

    Ok(order)
}

#[derive(Debug)]
pub enum GraphError {
    UnknownDependency {
        service: ServiceId,
        dependency: ServiceId,
    },
    SelfDependency {
        service: ServiceId,
    },
    Cycle {
        involved: Vec<ServiceId>,
    },
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDependency {
                service,
                dependency,
            } => write!(f, "service {service} depends on unknown {dependency}"),
            Self::SelfDependency { service } => write!(f, "service {service} depends on itself"),
            Self::Cycle { involved } => {
                let names: Vec<&str> = involved.iter().map(ServiceId::as_str).collect();
                write!(f, "dependency cycle: {}", names.join(", "))
            }
        }
    }
}

impl std::error::Error for GraphError {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::ServiceEntry;
    use crate::model::{ProbeSpec, ServiceKind, Severity};

    fn svc(id: &str, deps: &[&str]) -> ServiceEntry {
        ServiceEntry {
            id: ServiceId(id.to_string()),
            name: id.to_string(),
            unit: format!("{id}.service"),
            kind: ServiceKind::Systemd,
            probe: ProbeSpec::SystemdActive {
                unit: String::new(),
            },
            depends_on: deps.iter().map(|d| ServiceId(d.to_string())).collect(),
            resources: vec![],
            severity: Severity::Warning,
            startup_grace_secs: 10,
            restart_cooldown_secs: 60,
            max_restarts: 3,
            breaker_window_secs: 3600,
            breaker_cooldown_secs: 3600,
            backup_stop: false,
        }
    }

    #[test]
    fn linear_chain() {
        let services = vec![svc("adguard", &["unbound"]), svc("unbound", &[])];
        let g = DepGraph::build(&services).unwrap();
        let order = g.topological_order();
        let pos_unbound = order.iter().position(|id| id.as_str() == "unbound").unwrap();
        let pos_adguard = order.iter().position(|id| id.as_str() == "adguard").unwrap();
        assert!(pos_unbound < pos_adguard);
    }

    #[test]
    fn cycle_detected() {
        let services = vec![svc("a", &["b"]), svc("b", &["a"])];
        assert!(matches!(
            DepGraph::build(&services),
            Err(GraphError::Cycle { .. })
        ));
    }

    #[test]
    fn unknown_dependency() {
        let services = vec![svc("a", &["nonexistent"])];
        assert!(matches!(
            DepGraph::build(&services),
            Err(GraphError::UnknownDependency { .. })
        ));
    }

    #[test]
    fn self_dependency() {
        let services = vec![svc("a", &["a"])];
        assert!(matches!(
            DepGraph::build(&services),
            Err(GraphError::SelfDependency { .. })
        ));
    }

    #[test]
    fn root_cause_classification() {
        let services = vec![
            svc("docker_daemon", &[]),
            svc("continuwuity", &["docker_daemon"]),
            svc("gatus", &["docker_daemon"]),
            svc("unbound", &[]),
            svc("adguard", &["unbound"]),
        ];
        let g = DepGraph::build(&services).unwrap();

        let mut unhealthy = BTreeSet::new();
        unhealthy.insert(ServiceId("docker_daemon".into()));
        unhealthy.insert(ServiceId("continuwuity".into()));
        unhealthy.insert(ServiceId("gatus".into()));

        let (roots, blocked) = g.classify_failures(&unhealthy);

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].as_str(), "docker_daemon");
        assert_eq!(blocked.len(), 2);
        assert_eq!(
            blocked[&ServiceId("continuwuity".into())].as_str(),
            "docker_daemon"
        );
        assert_eq!(
            blocked[&ServiceId("gatus".into())].as_str(),
            "docker_daemon"
        );
    }

    #[test]
    fn independent_failures_are_all_roots() {
        let services = vec![svc("unbound", &[]), svc("ntfy", &[])];
        let g = DepGraph::build(&services).unwrap();

        let mut unhealthy = BTreeSet::new();
        unhealthy.insert(ServiceId("unbound".into()));
        unhealthy.insert(ServiceId("ntfy".into()));

        let (roots, blocked) = g.classify_failures(&unhealthy);
        assert_eq!(roots.len(), 2);
        assert!(blocked.is_empty());
    }
}
