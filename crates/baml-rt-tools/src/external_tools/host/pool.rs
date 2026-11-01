// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Runner-wide VM admission and idle LRU eviction. Busy VMs are never evicted.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use baml_rt_core::Result;
use tokio::sync::Mutex as AsyncMutex;

use super::{ToolGrant, TrustDomain, VmKey, channel::SupervisorChannel, invalid};
use crate::external_tools::{
    KtoolRuntimeImage,
    sandbox::{SandboxHandle, SandboxProvider},
};

/// Provider input contains only host-issued authority and a pinned runtime image.
#[derive(Debug, Clone)]
pub struct VmSpec {
    pub name: String,
    pub image: KtoolRuntimeImage,
    pub grant: Arc<ToolGrant>,
    pub memory_mib: u32,
    pub control_timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct VmBudget {
    pub count: usize,
    pub memory_mib: u64,
}

#[derive(Debug, Clone)]
pub struct PoolPolicy {
    pub total: VmBudget,
    pub per_domain: VmBudget,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
}

impl Default for PoolPolicy {
    fn default() -> Self {
        Self {
            total: VmBudget {
                count: 8,
                memory_mib: 4096,
            },
            per_domain: VmBudget {
                count: 4,
                memory_mib: 2048,
            },
            idle_timeout: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(3600),
        }
    }
}

struct Entry {
    handle: SandboxHandle,
    channel: SupervisorChannel,
    memory_mib: u32,
    created: Instant,
    last_used: Mutex<Instant>,
}

pub(super) struct VmLease(Arc<Entry>);

impl VmLease {
    pub fn channel(&self) -> &SupervisorChannel {
        &self.0.channel
    }
}

impl Drop for VmLease {
    fn drop(&mut self) {
        *self.0.last_used.lock().unwrap() = Instant::now();
    }
}

#[derive(Default)]
struct State {
    entries: HashMap<VmKey, Arc<Entry>>,
    closed: bool,
}

pub struct VmPool {
    provider: Arc<dyn SandboxProvider>,
    runner_id: String,
    policy: PoolPolicy,
    state: AsyncMutex<State>,
}

impl VmPool {
    pub fn new(
        provider: Arc<dyn SandboxProvider>,
        runner_id: impl Into<String>,
        policy: PoolPolicy,
    ) -> Arc<Self> {
        let pool = Arc::new(Self {
            provider,
            runner_id: runner_id.into(),
            policy,
            state: AsyncMutex::new(State::default()),
        });
        let weak = Arc::downgrade(&pool);
        // No strong reference crosses the timer wait, so dropping the runner releases the pool.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(pool) = weak.upgrade() else {
                    break;
                };
                if pool.prune().await.is_err() {
                    tracing::warn!("tool VM prune failed");
                }
                if pool.state.lock().await.closed {
                    break;
                }
            }
        });
        pool
    }

    pub(super) async fn acquire(
        self: &Arc<Self>,
        image: KtoolRuntimeImage,
        grant: Arc<ToolGrant>,
        memory_mib: u32,
        control_timeout: Duration,
    ) -> Result<VmLease> {
        let pool = self.clone();
        // Finish admission even if an invoking task is cancelled during VM creation.
        tokio::spawn(async move {
            pool.acquire_inner(image, grant, memory_mib, control_timeout)
                .await
        })
        .await
        .map_err(|_| invalid("VM admission task failed"))?
    }

    async fn acquire_inner(
        &self,
        image: KtoolRuntimeImage,
        grant: Arc<ToolGrant>,
        memory_mib: u32,
        control_timeout: Duration,
    ) -> Result<VmLease> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Err(invalid("tool VM pool is shut down"));
        }
        let key = grant.key().clone();
        self.prune_locked(&mut state).await?;
        if let Some(entry) = state.entries.get(&key) {
            if entry.channel.healthy() && entry.created.elapsed() < self.policy.max_lifetime {
                return Ok(VmLease(entry.clone()));
            }
            return Err(invalid("expired tool VM still has active calls"));
        }
        if memory_mib == 0
            || self.policy.total.count == 0
            || self.policy.per_domain.count == 0
            || u64::from(memory_mib)
                > self
                    .policy
                    .total
                    .memory_mib
                    .min(self.policy.per_domain.memory_mib)
        {
            return Err(invalid("tool VM exceeds pool budget"));
        }
        loop {
            let total = usage(&state, None);
            let domain = usage(&state, Some(key.trust_domain));
            let domain_full = full(domain, self.policy.per_domain, memory_mib);
            if !domain_full && !full(total, self.policy.total, memory_mib) {
                break;
            }
            let victim = state
                .entries
                .iter()
                .filter(|(k, e)| {
                    Arc::strong_count(e) == 1
                        && (!domain_full || k.trust_domain == key.trust_domain)
                })
                .min_by_key(|(_, e)| *e.last_used.lock().unwrap())
                .map(|(k, _)| k.clone())
                .ok_or_else(|| invalid("tool VM budget exhausted; all eligible VMs are busy"))?;
            self.evict(&mut state, &victim).await?;
        }
        let name = format!(
            "tool-{}",
            crate::mcp_snapshot::canonical_digest(&(&self.runner_id, &key))
                .to_string()
                .trim_start_matches("sha256:")
        );
        let spec = VmSpec {
            name,
            image,
            grant,
            memory_mib,
            control_timeout,
        };
        let (handle, channel) = self.provider.create_vm(spec).await?;
        let channel =
            match SupervisorChannel::connect(channel, key.grant_class, control_timeout).await {
                Ok(channel) => channel,
                Err(error) => {
                    self.provider.teardown(&handle).await?;
                    return Err(error);
                }
            };
        let entry = Arc::new(Entry {
            handle,
            channel,
            memory_mib,
            created: Instant::now(),
            last_used: Mutex::new(Instant::now()),
        });
        state.entries.insert(key, entry.clone());
        Ok(VmLease(entry))
    }

    async fn evict(&self, state: &mut State, key: &VmKey) -> Result<()> {
        if let Some(entry) = state.entries.get(key) {
            entry.channel.close();
            // Keep failed teardowns charged against the budget.
            self.provider.teardown(&entry.handle).await?;
        }
        state.entries.remove(key);
        Ok(())
    }

    async fn prune_locked(&self, state: &mut State) -> Result<()> {
        let expired: Vec<_> = state
            .entries
            .iter()
            .filter(|(_, e)| {
                Arc::strong_count(e) == 1
                    && (!e.channel.healthy()
                        || e.created.elapsed() >= self.policy.max_lifetime
                        || e.last_used.lock().unwrap().elapsed() >= self.policy.idle_timeout)
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            self.evict(state, &key).await?;
        }
        Ok(())
    }

    pub async fn prune(&self) -> Result<()> {
        self.prune_locked(&mut *self.state.lock().await).await
    }

    pub async fn usage(&self) -> VmBudget {
        usage(&*self.state.lock().await, None)
    }

    pub async fn shutdown(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.closed = true;
        let keys: Vec<_> = state.entries.keys().cloned().collect();
        let mut failure = None;
        for key in keys {
            if let Err(error) = self.evict(&mut state, &key).await {
                failure = Some(error);
            }
        }
        if let Err(error) = self.provider.clear_vm_templates().await {
            failure = Some(error);
        }
        failure.map_or(Ok(()), Err)
    }
}

fn usage(state: &State, domain: Option<TrustDomain>) -> VmBudget {
    let mut usage = VmBudget {
        count: 0,
        memory_mib: 0,
    };
    for (key, entry) in &state.entries {
        if domain.is_none_or(|d| d == key.trust_domain) {
            usage.count += 1;
            usage.memory_mib += u64::from(entry.memory_mib);
        }
    }
    usage
}

fn full(used: VmBudget, budget: VmBudget, memory_mib: u32) -> bool {
    used.count >= budget.count || used.memory_mib + u64::from(memory_mib) > budget.memory_mib
}
