//! Resource lease arbiter for mutual exclusion of conflicting operations.
//!
//! Simple, in-memory, no persistence. Leases are lost on restart —
//! which is correct because all operations are also lost on restart.
//!
//! Not thread-safe — owned by the control loop.

use crate::model::ResourceId;
use std::collections::BTreeMap;

/// Tracks which resources are currently held and by whom.
#[derive(Debug)]
pub struct LeaseArbiter {
    held: BTreeMap<ResourceId, LeaseEntry>,
    /// Maximum duration a lease can be held before auto-expiry (safety net).
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
    /// Lease acquired successfully.
    Granted,
    /// Resource already held by another holder.
    Conflict { current_holder: String },
    /// Expired lease was evicted, new lease granted.
    GrantedAfterEviction { previous_holder: String },
}

impl LeaseArbiter {
    /// Creates a new arbiter with the given maximum TTL for any lease.
    #[must_use]
    pub fn new(max_ttl_secs: u64) -> Self {
        Self {
            held: BTreeMap::new(),
            max_ttl_secs,
        }
    }

    /// Attempts to acquire a lease on a resource.
    ///
    /// `now_mono_secs` is the current monotonic time in seconds.
    ///
    /// If the same holder already holds the lease, this is a no-op (idempotent).
    pub fn acquire(
        &mut self,
        resource: ResourceId,
        holder: &str,
        now_mono_secs: u64,
    ) -> AcquireResult {
        if let Some(entry) = self.held.get(&resource) {
            // Same holder — idempotent.
            if entry.holder == holder {
                return AcquireResult::Granted;
            }

            // Check if existing lease expired.
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

    /// Releases a lease on a resource.
    ///
    /// If the resource is not held, or held by a different holder, this is a no-op.
    /// Returns `true` if the lease was actually released.
    pub fn release(&mut self, resource: &ResourceId, holder: &str) -> bool {
        if let Some(entry) = self.held.get(resource) {
            if entry.holder == holder {
                self.held.remove(resource);
                return true;
            }
        }
        false
    }

    /// Checks if a resource is currently held (and by whom).
    #[must_use]
    pub fn holder(&self, resource: &ResourceId) -> Option<&str> {
        self.held.get(resource).map(|e| e.holder.as_str())
    }

    /// Returns `true` if the resource is free (not held or expired).
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

    /// Evicts all expired leases. Called periodically by control loop.
    pub fn evict_expired(&mut self, now_mono_secs: u64) {
        self.held.retain(|_resource, entry| {
            let age = now_mono_secs.saturating_sub(entry.acquired_mono_secs);
            age < self.max_ttl_secs
        });
    }

    /// Returns how many leases are currently held (including expired).
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
        let mut arb = LeaseArbiter::new(100); // 100 second TTL
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
        // "a" at t=0, ttl=100, age=100 → expired
        // "b" at t=50, ttl=100, age=50 → alive
        // "c" at t=90, ttl=100, age=10 → alive
        assert_eq!(arb.held_count(), 2);
    }
}