//! Resource lease arbiter for mutual exclusion of conflicting operations.
//!
//! In-memory, no persistence. Leases lost on restart — correct because
//! all in-flight operations are also lost on restart.

use crate::model::ResourceId;
use std::collections::BTreeMap;

/// Tracks which resources are currently held and by whom.
#[derive(Debug)]
pub struct LeaseArbiter {
    held: BTreeMap<ResourceId, LeaseEntry>,
    max_ttl_secs: u64,
}

#[derive(Debug, Clone)]
struct LeaseEntry {
    holder: String,
    acquired_mono_secs: u64,
}

/// Result of trying to acquire a lease.
#[derive(Debug, PartialEq, Eq)]
pub enum AcquireResult {
    Granted,
    Conflict { current_holder: String },
    GrantedAfterEviction { previous_holder: String },
}

impl LeaseArbiter {
    #[must_use]
    pub fn new(max_ttl_secs: u64) -> Self {
        Self {
            held: BTreeMap::new(),
            max_ttl_secs,
        }
    }

    pub fn acquire(
        &mut self,
        resource: ResourceId,
        holder: &str,
        now_mono_secs: u64,
    ) -> AcquireResult {
        if let Some(entry) = self.held.get(&resource) {
            if entry.holder == holder {
                return AcquireResult::Granted;
            }

            let age = now_mono_secs.saturating_sub(entry.acquired_mono_secs);
            if age >= self.max_ttl_secs {
                let previous = entry.holder.clone();
                self.held.insert(
                    resource,
                    LeaseEntry {
                        holder: holder.to_string(),
                        acquired_mono_secs: now_mono_secs,
                    },
                );
                return AcquireResult::GrantedAfterEviction {
                    previous_holder: previous,
                };
            }

            return AcquireResult::Conflict {
                current_holder: entry.holder.clone(),
            };
        }

        self.held.insert(
            resource,
            LeaseEntry {
                holder: holder.to_string(),
                acquired_mono_secs: now_mono_secs,
            },
        );
        AcquireResult::Granted
    }

    /// Unconditionally releases a lease regardless of holder.
    /// Used by the effect executor when the reducer emits `ReleaseLease`.
    pub fn force_release(&mut self, resource: &ResourceId) {
        self.held.remove(resource);
    }

    #[must_use]
    pub fn is_free(&self, resource: &ResourceId, now_mono_secs: u64) -> bool {
        match self.held.get(resource) {
            None => true,
            Some(entry) => {
                let age = now_mono_secs.saturating_sub(entry.acquired_mono_secs);
                age >= self.max_ttl_secs
            }
        }
    }

}

#[cfg(test)]
impl LeaseArbiter {
    pub fn release(&mut self, resource: &ResourceId, holder: &str) -> bool {
        if let Some(entry) = self.held.get(resource) {
            if entry.holder == holder {
                self.held.remove(resource);
                return true;
            }
        }
        false
    }

    #[must_use]
    pub fn holder(&self, resource: &ResourceId) -> Option<&str> {
        self.held.get(resource).map(|e| e.holder.as_str())
    }

    pub fn evict_expired(&mut self, now_mono_secs: u64) {
        self.held.retain(|_resource, entry| {
            let age = now_mono_secs.saturating_sub(entry.acquired_mono_secs);
            age < self.max_ttl_secs
        });
    }

    #[must_use]
    pub fn held_count(&self) -> usize {
        self.held.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::{ResourceId, ServiceId};

    fn svc_resource(name: &str) -> ResourceId {
        ResourceId::Service(ServiceId(name.to_string()))
    }

    #[test]
    fn acquire_and_release() {
        let mut arb = LeaseArbiter::new(3600);
        let r = svc_resource("continuwuity");

        assert!(arb.is_free(&r, 0));

        let result = arb.acquire(r.clone(), "backup", 100);
        assert_eq!(result, AcquireResult::Granted);
        assert!(!arb.is_free(&r, 100));
        assert_eq!(arb.holder(&r), Some("backup"));

        assert!(arb.release(&r, "backup"));
        assert!(arb.is_free(&r, 100));
    }

    #[test]
    fn conflict() {
        let mut arb = LeaseArbiter::new(3600);
        let r = ResourceId::DockerDaemon;

        arb.acquire(r.clone(), "recovery", 0);
        let result = arb.acquire(r.clone(), "remediation", 10);
        assert_eq!(
            result,
            AcquireResult::Conflict {
                current_holder: "recovery".to_string()
            }
        );
    }

    #[test]
    fn idempotent_acquire() {
        let mut arb = LeaseArbiter::new(3600);
        let r = ResourceId::BackupRepo;

        arb.acquire(r.clone(), "backup", 0);
        let result = arb.acquire(r.clone(), "backup", 5);
        assert_eq!(result, AcquireResult::Granted);
    }

    #[test]
    fn expired_lease_evicted() {
        let mut arb = LeaseArbiter::new(100);
        let r = ResourceId::DockerDaemon;

        arb.acquire(r.clone(), "old_holder", 0);
        assert!(!arb.is_free(&r, 50));
        assert!(arb.is_free(&r, 100));

        let result = arb.acquire(r.clone(), "new_holder", 100);
        assert_eq!(
            result,
            AcquireResult::GrantedAfterEviction {
                previous_holder: "old_holder".to_string()
            }
        );
    }

    #[test]
    fn wrong_holder_release_is_noop() {
        let mut arb = LeaseArbiter::new(3600);
        let r = svc_resource("ntfy");

        arb.acquire(r.clone(), "backup", 0);
        assert!(!arb.release(&r, "someone_else"));
        assert!(!arb.is_free(&r, 0));
    }

    #[test]
    fn evict_expired() {
        let mut arb = LeaseArbiter::new(100);

        arb.acquire(ResourceId::DockerDaemon, "a", 0);
        arb.acquire(ResourceId::BackupRepo, "b", 50);
        arb.acquire(ResourceId::DiskCleanup, "c", 90);

        assert_eq!(arb.held_count(), 3);

        arb.evict_expired(100);
        assert_eq!(arb.held_count(), 2);
    }
}
