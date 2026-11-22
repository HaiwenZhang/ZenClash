use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use zenclash_core::RuleCatalog;

use super::{RuleFilterSelection, rule_matches};

pub(super) struct RuleProjection {
    pub(super) snapshot: Arc<RuleCatalog>,
    pub(super) filter: RuleFilterSelection,
    pub(super) confirmed_disabled: HashMap<usize, bool>,
    pub(super) indices: Vec<usize>,
    pub(super) types: Vec<String>,
    pub(super) policies: Vec<String>,
}

pub(super) fn rule_is_disabled(
    rule: &zenclash_core::Rule,
    confirmed_disabled: &HashMap<usize, bool>,
) -> bool {
    rule.index
        .and_then(|index| confirmed_disabled.get(&index).copied())
        .unwrap_or_else(|| rule.extra.as_ref().is_some_and(|stats| stats.disabled))
}

#[derive(Default)]
pub(super) struct ProjectionWorker {
    generation: Arc<AtomicU64>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: crate::runtime_host::observation::PageReadTask,
}

impl ProjectionWorker {
    #[cfg(test)]
    pub(super) fn hold_projection(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.gate
            .clone()
            .try_lock_owned()
            .expect("projection must be idle")
    }

    pub(super) fn cancel(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.task.cancel();
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }

    pub(super) fn start(
        &mut self,
        runtime: &tokio::runtime::Handle,
        snapshot: Arc<RuleCatalog>,
        filter: RuleFilterSelection,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<RuleProjection>, String>>,
    ) {
        self.start_with_options(
            runtime,
            snapshot,
            filter,
            HashMap::new(),
            Duration::from_millis(80),
        )
    }

    pub(super) fn start_with_options(
        &mut self,
        runtime: &tokio::runtime::Handle,
        snapshot: Arc<RuleCatalog>,
        filter: RuleFilterSelection,
        confirmed_disabled: HashMap<usize, bool>,
        debounce: Duration,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<RuleProjection>, String>>,
    ) {
        self.cancel();
        let generation = self.generation.load(Ordering::Acquire);
        let current = self.generation.clone();
        let gate = self.gate.clone();

        let task = runtime.spawn(async move {
            if !debounce.is_zero() {
                tokio::time::sleep(debounce).await;
            }
            let permit = gate.lock_owned().await;
            if current.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let (indices, types, policies) = {
                    let mut indices = Vec::new();
                    let mut kinds = std::collections::BTreeSet::<&str>::new();
                    let mut policies = std::collections::BTreeSet::<&str>::new();
                    for (index, rule) in snapshot.rules.iter().enumerate() {
                        if index % 256 == 0 && current.load(Ordering::Acquire) != generation {
                            return None;
                        }
                        policies.insert(rule.proxy.as_str());
                        if (!filter.disabled_only || rule_is_disabled(rule, &confirmed_disabled))
                            && rule_matches(rule, &filter.query)
                            && filter
                                .rule_kind
                                .as_ref()
                                .is_none_or(|kind| *kind == rule.kind)
                            && filter
                                .policy
                                .as_ref()
                                .is_none_or(|policy| *policy == rule.proxy)
                        {
                            indices.push(index);
                        }
                        kinds.insert(rule.kind.as_str());
                    }
                    (
                        indices,
                        kinds.into_iter().map(str::to_owned).collect(),
                        policies.into_iter().map(str::to_owned).collect(),
                    )
                };
                (current.load(Ordering::Acquire) == generation).then_some(RuleProjection {
                    snapshot,
                    filter,
                    confirmed_disabled,
                    indices,
                    types,
                    policies,
                })
            })
            .await
            .map_err(|error| error.to_string())
        });
        self.task.replace(&task);
        (generation, task)
    }
}

impl Drop for ProjectionWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests;
