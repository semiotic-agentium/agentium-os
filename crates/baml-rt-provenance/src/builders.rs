use crate::{
    document::ProvDocument,
    types::{
        Activity, Agent, Entity, ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef,
        QualifiedGeneration, Used, WasAssociatedWith, WasDerivedFrom, WasGeneratedBy,
    },
};
use std::collections::HashMap;

pub struct EntityBuilder {
    id: ProvEntityId,
    prov_type: Option<String>,
    attributes: HashMap<String, serde_json::Value>,
}

impl EntityBuilder {
    pub fn new(id: impl Into<ProvEntityId>) -> Self {
        Self {
            id: id.into(),
            prov_type: None,
            attributes: HashMap::new(),
        }
    }

    pub fn type_(mut self, prov_type: &str) -> Self {
        self.prov_type = Some(prov_type.to_string());
        self
    }

    pub fn attr(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.attributes.insert(key.to_string(), value.into());
        self
    }

    pub fn build(self) -> (ProvEntityId, Entity) {
        let entity = Entity {
            prov_type: self.prov_type,
            attributes: self.attributes,
        };
        (self.id, entity)
    }
}

pub struct ActivityBuilder {
    id: ProvActivityId,
    start_time_ms: Option<u64>,
    end_time_ms: Option<u64>,
    prov_type: Option<String>,
    attributes: HashMap<String, serde_json::Value>,
}

impl ActivityBuilder {
    pub fn new(id: impl Into<ProvActivityId>) -> Self {
        Self {
            id: id.into(),
            start_time_ms: None,
            end_time_ms: None,
            prov_type: None,
            attributes: HashMap::new(),
        }
    }

    pub fn start_time_ms(mut self, time_ms: u64) -> Self {
        self.start_time_ms = Some(time_ms);
        self
    }

    pub fn end_time_ms(mut self, time_ms: u64) -> Self {
        self.end_time_ms = Some(time_ms);
        self
    }

    pub fn type_(mut self, prov_type: &str) -> Self {
        self.prov_type = Some(prov_type.to_string());
        self
    }

    pub fn attr(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.attributes.insert(key.to_string(), value.into());
        self
    }

    pub fn build(self) -> (ProvActivityId, Activity) {
        let activity = Activity {
            start_time_ms: self.start_time_ms,
            end_time_ms: self.end_time_ms,
            prov_type: self.prov_type,
            attributes: self.attributes,
        };
        (self.id, activity)
    }
}

pub struct AgentBuilder {
    id: ProvAgentId,
    prov_type: Option<String>,
    attributes: HashMap<String, serde_json::Value>,
}

impl AgentBuilder {
    pub fn new(id: impl Into<ProvAgentId>) -> Self {
        Self {
            id: id.into(),
            prov_type: None,
            attributes: HashMap::new(),
        }
    }

    pub fn type_(mut self, prov_type: &str) -> Self {
        self.prov_type = Some(prov_type.to_string());
        self
    }

    pub fn attr(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.attributes.insert(key.to_string(), value.into());
        self
    }

    pub fn build(self) -> (ProvAgentId, Agent) {
        let agent = Agent {
            prov_type: self.prov_type,
            attributes: self.attributes,
        };
        (self.id, agent)
    }
}

pub struct ProvDocumentBuilder {
    doc: ProvDocument,
}

impl ProvDocumentBuilder {
    pub fn new() -> Self {
        Self {
            doc: ProvDocument::new(),
        }
    }

    pub fn entity<F>(mut self, id: impl Into<ProvEntityId>, f: F) -> Self
    where
        F: FnOnce(EntityBuilder) -> (ProvEntityId, Entity),
    {
        let (id, entity) = f(EntityBuilder::new(id));
        self.doc.insert_entity(id, entity);
        self
    }

    pub fn activity<F>(mut self, id: impl Into<ProvActivityId>, f: F) -> Self
    where
        F: FnOnce(ActivityBuilder) -> (ProvActivityId, Activity),
    {
        let (id, activity) = f(ActivityBuilder::new(id));
        self.doc.insert_activity(id, activity);
        self
    }

    pub fn agent<F>(mut self, id: impl Into<ProvAgentId>, f: F) -> Self
    where
        F: FnOnce(AgentBuilder) -> (ProvAgentId, Agent),
    {
        let (id, agent) = f(AgentBuilder::new(id));
        self.doc.insert_agent(id, agent);
        self
    }

    pub fn used(
        self,
        activity: impl Into<ProvActivityId>,
        entity: impl Into<ProvEntityId>,
    ) -> UsedBuilder {
        UsedBuilder::new(self, activity.into(), entity.into())
    }

    pub fn was_generated_by(
        self,
        entity: impl Into<ProvNodeRef>,
        activity: impl Into<ProvActivityId>,
    ) -> WasGeneratedByBuilder {
        WasGeneratedByBuilder::new(self, entity.into(), activity.into())
    }

    pub fn qualified_generation(
        self,
        entity: impl Into<ProvNodeRef>,
        activity: impl Into<ProvActivityId>,
    ) -> QualifiedGenerationBuilder {
        QualifiedGenerationBuilder::new(self, entity.into(), activity.into())
    }

    pub fn was_associated_with(
        self,
        activity: impl Into<ProvActivityId>,
        agent: impl Into<ProvAgentId>,
    ) -> WasAssociatedWithBuilder {
        WasAssociatedWithBuilder::new(self, activity.into(), agent.into())
    }

    pub fn was_derived_from(
        self,
        generated_entity: impl Into<ProvEntityId>,
        used_entity: impl Into<ProvEntityId>,
    ) -> WasDerivedFromBuilder {
        WasDerivedFromBuilder::new(self, generated_entity.into(), used_entity.into())
    }

    pub fn build(self) -> ProvDocument {
        self.doc
    }
}

pub struct UsedBuilder {
    doc_builder: ProvDocumentBuilder,
    activity: ProvActivityId,
    entity: ProvEntityId,
    role: Option<String>,
}

impl UsedBuilder {
    fn new(
        doc_builder: ProvDocumentBuilder,
        activity: ProvActivityId,
        entity: ProvEntityId,
    ) -> Self {
        Self {
            doc_builder,
            activity,
            entity,
            role: None,
        }
    }

    pub fn role(mut self, role: &str) -> Self {
        self.role = Some(role.to_string());
        self
    }

    pub fn build(mut self) -> ProvDocumentBuilder {
        let id = self.doc_builder.doc.blank_node_id("u");
        let used = Used {
            activity: self.activity,
            entity: self.entity,
            role: self.role,
        };
        self.doc_builder.doc.insert_used(id, used);
        self.doc_builder
    }
}

pub struct WasGeneratedByBuilder {
    doc_builder: ProvDocumentBuilder,
    entity: ProvNodeRef,
    activity: ProvActivityId,
    time_ms: Option<u64>,
}

pub struct QualifiedGenerationBuilder {
    doc_builder: ProvDocumentBuilder,
    entity: ProvNodeRef,
    activity: ProvActivityId,
    time_ms: Option<u64>,
}

pub struct WasAssociatedWithBuilder {
    doc_builder: ProvDocumentBuilder,
    activity: ProvActivityId,
    agent: ProvAgentId,
    role: Option<String>,
}

impl WasAssociatedWithBuilder {
    fn new(doc_builder: ProvDocumentBuilder, activity: ProvActivityId, agent: ProvAgentId) -> Self {
        Self {
            doc_builder,
            activity,
            agent,
            role: None,
        }
    }

    pub fn role(mut self, role: &str) -> Self {
        self.role = Some(role.to_string());
        self
    }

    pub fn build(mut self) -> ProvDocumentBuilder {
        let id = self.doc_builder.doc.blank_node_id("assoc");
        let was_associated_with = WasAssociatedWith {
            activity: self.activity,
            agent: self.agent,
            role: self.role,
        };
        self.doc_builder
            .doc
            .insert_was_associated_with(id, was_associated_with);
        self.doc_builder
    }
}

impl WasGeneratedByBuilder {
    fn new(
        doc_builder: ProvDocumentBuilder,
        entity: ProvNodeRef,
        activity: ProvActivityId,
    ) -> Self {
        Self {
            doc_builder,
            entity,
            activity,
            time_ms: None,
        }
    }

    pub fn time_ms(mut self, time_ms: u64) -> Self {
        self.time_ms = Some(time_ms);
        self
    }

    pub fn build(mut self) -> ProvDocumentBuilder {
        let id = self.doc_builder.doc.blank_node_id("g");
        let was_generated_by = WasGeneratedBy {
            entity: self.entity,
            activity: self.activity,
            time_ms: self.time_ms,
        };
        self.doc_builder
            .doc
            .insert_was_generated_by(id, was_generated_by);
        self.doc_builder
    }
}

impl QualifiedGenerationBuilder {
    fn new(
        doc_builder: ProvDocumentBuilder,
        entity: ProvNodeRef,
        activity: ProvActivityId,
    ) -> Self {
        Self {
            doc_builder,
            entity,
            activity,
            time_ms: None,
        }
    }

    pub fn time_ms(mut self, time_ms: u64) -> Self {
        self.time_ms = Some(time_ms);
        self
    }

    pub fn build(mut self) -> ProvDocumentBuilder {
        let id = self.doc_builder.doc.blank_node_id("gen");
        let qualified_generation = QualifiedGeneration {
            entity: self.entity,
            activity: self.activity,
            time_ms: self.time_ms,
        };
        self.doc_builder
            .doc
            .insert_qualified_generation(id, qualified_generation);
        self.doc_builder
    }
}

pub struct WasDerivedFromBuilder {
    doc_builder: ProvDocumentBuilder,
    generated_entity: ProvEntityId,
    used_entity: ProvEntityId,
    activity: Option<ProvActivityId>,
    prov_type: Option<String>,
}

impl WasDerivedFromBuilder {
    fn new(
        doc_builder: ProvDocumentBuilder,
        generated_entity: ProvEntityId,
        used_entity: ProvEntityId,
    ) -> Self {
        Self {
            doc_builder,
            generated_entity,
            used_entity,
            activity: None,
            prov_type: None,
        }
    }

    pub fn activity(mut self, activity: impl Into<ProvActivityId>) -> Self {
        self.activity = Some(activity.into());
        self
    }

    pub fn type_(mut self, prov_type: &str) -> Self {
        self.prov_type = Some(prov_type.to_string());
        self
    }

    pub fn build(mut self) -> ProvDocumentBuilder {
        let id = self.doc_builder.doc.blank_node_id("d");
        let was_derived_from = WasDerivedFrom {
            generated_entity: self.generated_entity,
            used_entity: self.used_entity,
            activity: self.activity,
            prov_type: self.prov_type,
        };
        self.doc_builder
            .doc
            .insert_was_derived_from(id, was_derived_from);
        self.doc_builder
    }
}

impl Default for ProvDocumentBuilder {
    fn default() -> Self {
        Self::new()
    }
}
