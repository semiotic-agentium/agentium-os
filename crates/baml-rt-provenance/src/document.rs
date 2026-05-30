// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use crate::types::{
    Activity, Agent, Entity, ProvActivityId, ProvAgentId, ProvEntityId, QualifiedGeneration, Used,
    WasAssociatedWith, WasDerivedFrom, WasGeneratedBy,
};

#[derive(Debug, Clone, Default)]
pub struct ProvDocument {
    entity: HashMap<ProvEntityId, Entity>,
    activity: HashMap<ProvActivityId, Activity>,
    agent: HashMap<ProvAgentId, Agent>,
    used: HashMap<String, Used>,
    was_generated_by: HashMap<String, WasGeneratedBy>,
    qualified_generation: HashMap<String, QualifiedGeneration>,
    was_associated_with: HashMap<String, WasAssociatedWith>,
    was_derived_from: HashMap<String, WasDerivedFrom>,
    blank_node_counter: u64,
}

impl ProvDocument {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_entity(&mut self, id: ProvEntityId, entity: Entity) {
        self.entity.insert(id, entity);
    }

    pub fn insert_activity(&mut self, id: ProvActivityId, activity: Activity) {
        self.activity.insert(id, activity);
    }

    pub fn insert_agent(&mut self, id: ProvAgentId, agent: Agent) {
        self.agent.insert(id, agent);
    }

    pub fn insert_used(&mut self, id: String, used: Used) {
        self.used.insert(id, used);
    }

    pub fn insert_was_generated_by(&mut self, id: String, rel: WasGeneratedBy) {
        self.was_generated_by.insert(id, rel);
    }

    pub fn insert_qualified_generation(&mut self, id: String, rel: QualifiedGeneration) {
        self.qualified_generation.insert(id, rel);
    }

    pub fn insert_was_associated_with(&mut self, id: String, rel: WasAssociatedWith) {
        self.was_associated_with.insert(id, rel);
    }

    pub fn insert_was_derived_from(&mut self, id: String, rel: WasDerivedFrom) {
        self.was_derived_from.insert(id, rel);
    }

    pub fn entities(&self) -> impl Iterator<Item = (&ProvEntityId, &Entity)> {
        self.entity.iter()
    }

    pub fn activities(&self) -> impl Iterator<Item = (&ProvActivityId, &Activity)> {
        self.activity.iter()
    }

    pub fn agents(&self) -> impl Iterator<Item = (&ProvAgentId, &Agent)> {
        self.agent.iter()
    }

    pub fn used(&self) -> impl Iterator<Item = (&String, &Used)> {
        self.used.iter()
    }

    pub fn was_generated_by(&self) -> impl Iterator<Item = (&String, &WasGeneratedBy)> {
        self.was_generated_by.iter()
    }

    pub fn qualified_generation(&self) -> impl Iterator<Item = (&String, &QualifiedGeneration)> {
        self.qualified_generation.iter()
    }

    pub fn was_associated_with(&self) -> impl Iterator<Item = (&String, &WasAssociatedWith)> {
        self.was_associated_with.iter()
    }

    pub fn was_derived_from(&self) -> impl Iterator<Item = (&String, &WasDerivedFrom)> {
        self.was_derived_from.iter()
    }

    pub fn entity(&self, id: &ProvEntityId) -> Option<&Entity> {
        self.entity.get(id)
    }

    pub fn activity(&self, id: &ProvActivityId) -> Option<&Activity> {
        self.activity.get(id)
    }

    pub fn agent(&self, id: &ProvAgentId) -> Option<&Agent> {
        self.agent.get(id)
    }

    pub fn blank_node_id(&mut self, prefix: &str) -> String {
        self.blank_node_counter += 1;
        format!("{}{}", prefix, self.blank_node_counter)
    }
}
