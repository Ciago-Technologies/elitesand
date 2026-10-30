//! Owner-local incarnation-bound reverse links and bounded, deterministic cascade plans.
use crate::{ActorRef, Error, Result};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDelete {
    Restrict,
    Cascade,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub dependent: ActorRef,
    pub target: ActorRef,
    pub on_delete: OnDelete,
}
#[derive(Default)]
pub struct Links {
    incoming: BTreeMap<ActorRef, BTreeMap<ActorRef, OnDelete>>,
    outgoing: BTreeMap<ActorRef, BTreeSet<ActorRef>>,
    count: usize,
}
impl Links {
    pub fn count(&self) -> usize {
        self.count
    }
    pub fn contains(&self, dependent: ActorRef, target: ActorRef) -> bool {
        self.outgoing
            .get(&dependent)
            .is_some_and(|s| s.contains(&target))
    }
    pub fn add(&mut self, e: Edge) {
        if self
            .incoming
            .entry(e.target)
            .or_default()
            .insert(e.dependent, e.on_delete)
            .is_none()
        {
            self.count += 1;
        }
        self.outgoing
            .entry(e.dependent)
            .or_default()
            .insert(e.target);
    }
    pub fn plan_delete(&self, root: ActorRef, limit: usize) -> Result<Vec<ActorRef>> {
        let mut seen = BTreeSet::new();
        let mut pending = VecDeque::from([root]);
        while let Some(target) = pending.pop_front() {
            if !seen.insert(target) {
                continue;
            }
            if seen.len() > limit {
                return Err(Error::Budget);
            }
            if let Some(edges) = self.incoming.get(&target) {
                for (&dependent, mode) in edges {
                    if *mode == OnDelete::Cascade {
                        pending.push_back(dependent);
                    }
                }
            }
            if pending.len() > self.count.saturating_add(1) {
                return Err(Error::Budget);
            }
        }
        for target in &seen {
            if let Some(edges) = self.incoming.get(target) {
                for (dependent, mode) in edges {
                    if *mode == OnDelete::Restrict && !seen.contains(dependent) {
                        return Err(Error::Restricted);
                    }
                }
            }
        }
        Ok(seen.into_iter().collect())
    }
    pub fn remove_actor(&mut self, actor: ActorRef) {
        if let Some(targets) = self.outgoing.remove(&actor) {
            for target in targets {
                if let Some(edges) = self.incoming.get_mut(&target) {
                    if edges.remove(&actor).is_some() {
                        self.count -= 1
                    }
                    if edges.is_empty() {
                        self.incoming.remove(&target);
                    }
                }
            }
        }
        if let Some(dependents) = self.incoming.remove(&actor) {
            for (dependent, _) in dependents {
                self.count -= 1;
                if let Some(targets) = self.outgoing.get_mut(&dependent) {
                    targets.remove(&actor);
                    if targets.is_empty() {
                        self.outgoing.remove(&dependent);
                    }
                }
            }
        }
    }
}
