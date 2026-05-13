//! Compose multiple `ExternalToolResolver` instances into one.
//!
//! Lookups query each inner resolver in registration order. Collisions
//! between resolvers fail closed — the same tool name may not be supplied
//! by more than one resolver. This mirrors the build-time collision policy
//! enforced inside `build_builder_catalog`.

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ExternalToolResolver,
    tools::{ToolFunctionMetadata, ToolHandler, ToolName},
};

#[derive(Default)]
pub struct CompositeResolver {
    inner: Vec<Box<dyn ExternalToolResolver>>,
}

impl CompositeResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, resolver: Box<dyn ExternalToolResolver>) -> Self {
        self.inner.push(resolver);
        self
    }

    pub fn push(&mut self, resolver: Box<dyn ExternalToolResolver>) {
        self.inner.push(resolver);
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

type ResolvedTool = (ToolFunctionMetadata, Arc<dyn ToolHandler>);

impl ExternalToolResolver for CompositeResolver {
    fn resolve(&self, name: &ToolName) -> Result<Option<ResolvedTool>> {
        let mut hit: Option<(usize, ResolvedTool)> = None;
        for (idx, resolver) in self.inner.iter().enumerate() {
            if let Some(found) = resolver.resolve(name)? {
                if let Some((first_idx, _)) = &hit {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "Tool name `{name}` resolved by more than one external resolver (slot {first_idx} and slot {idx})"
                    )));
                }
                hit = Some((idx, found));
            }
        }
        Ok(hit.map(|(_, value)| value))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use baml_rt_tools::{
        ToolSession,
        tool_fsm::ToolStep,
        tools::{SessionPolicy, ToolBackend, ToolOrigin, ToolSessionContext, ToolTypeSpec},
    };
    use serde_json::Value;

    use super::*;

    struct StubResolver {
        registered: std::collections::HashMap<ToolName, ToolFunctionMetadata>,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl StubResolver {
        fn new(names: &[&str], log: Arc<Mutex<Vec<String>>>, label: &str) -> Self {
            let mut registered = std::collections::HashMap::new();
            for name in names {
                let parsed = ToolName::parse(name).unwrap();
                let class =
                    ToolFunctionMetadata::derive_class_name(parsed.bundle(), parsed.local());
                registered.insert(
                    parsed.clone(),
                    ToolFunctionMetadata {
                        name: parsed,
                        class_name: class.clone(),
                        description: format!("{label}:{name}"),
                        open_input_schema: serde_json::json!({}),
                        input_schema: serde_json::json!({}),
                        output_schema: serde_json::json!({}),
                        open_input_type: ToolTypeSpec {
                            name: "()".into(),
                            ts_decl: None,
                        },
                        input_type: ToolTypeSpec {
                            name: format!("{class}Input"),
                            ts_decl: None,
                        },
                        output_type: ToolTypeSpec {
                            name: format!("{class}Output"),
                            ts_decl: None,
                        },
                        baml_decl: None,
                        extra_ts_decls: Vec::new(),
                        access: None,
                        tags: Vec::new(),
                        secret_requests: Vec::new(),
                        config: None,
                        config_bundle: None,
                        origin: ToolOrigin::Host,
                        backend: ToolBackend::External,
                        digest: None,
                        projection_semantics: None,
                        session_policy: SessionPolicy::Strict,
                        event_sources: Vec::new(),
                        coordination_baml: None,
                    },
                );
            }
            Self { registered, log }
        }
    }

    struct NoopHandler {
        metadata: ToolFunctionMetadata,
    }

    #[async_trait::async_trait]
    impl ToolHandler for NoopHandler {
        fn metadata(&self) -> &ToolFunctionMetadata {
            &self.metadata
        }
        async fn open_session(
            &self,
            _ctx: ToolSessionContext,
            _open_input: Value,
        ) -> Result<Box<dyn ToolSession>> {
            unimplemented!("test stub")
        }
    }

    #[async_trait::async_trait]
    impl ToolSession for NoopHandler {
        async fn send(
            &mut self,
            _input: Value,
        ) -> std::result::Result<(), baml_rt_tools::tool_fsm::ToolSessionError> {
            unimplemented!()
        }
        async fn read(
            &mut self,
            _input: Value,
        ) -> std::result::Result<ToolStep, baml_rt_tools::tool_fsm::ToolSessionError> {
            unimplemented!()
        }
        async fn finish(
            &mut self,
        ) -> std::result::Result<(), baml_rt_tools::tool_fsm::ToolSessionError> {
            unimplemented!()
        }
        async fn abort(
            &mut self,
            _reason: Option<String>,
        ) -> std::result::Result<(), baml_rt_tools::tool_fsm::ToolSessionError> {
            unimplemented!()
        }
    }

    impl ExternalToolResolver for StubResolver {
        fn resolve(
            &self,
            name: &ToolName,
        ) -> Result<Option<(ToolFunctionMetadata, Arc<dyn ToolHandler>)>> {
            self.log.lock().unwrap().push(name.to_string());
            match self.registered.get(name) {
                Some(metadata) => Ok(Some((
                    metadata.clone(),
                    Arc::new(NoopHandler {
                        metadata: metadata.clone(),
                    }) as Arc<dyn ToolHandler>,
                ))),
                None => Ok(None),
            }
        }
    }

    #[test]
    fn first_matching_resolver_wins_when_no_collision() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let a = StubResolver::new(&["mcp/grafana/search"], log.clone(), "a");
        let b = StubResolver::new(&["support/echo"], log.clone(), "b");
        let composite = CompositeResolver::new().with(Box::new(a)).with(Box::new(b));
        let resolved = composite
            .resolve(&ToolName::parse("mcp/grafana/search").unwrap())
            .unwrap()
            .expect("resolved");
        assert_eq!(resolved.0.description, "a:mcp/grafana/search");
    }

    #[test]
    fn duplicate_match_fails_closed() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let a = StubResolver::new(&["mcp/grafana/search"], log.clone(), "a");
        let b = StubResolver::new(&["mcp/grafana/search"], log.clone(), "b");
        let composite = CompositeResolver::new().with(Box::new(a)).with(Box::new(b));
        match composite.resolve(&ToolName::parse("mcp/grafana/search").unwrap()) {
            Err(err) => assert!(
                err.to_string().contains("resolved by more than one"),
                "unexpected error: {err}"
            ),
            Ok(_) => panic!("expected collision error"),
        }
    }

    #[test]
    fn missing_returns_none_without_error() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let a = StubResolver::new(&["mcp/grafana/search"], log.clone(), "a");
        let composite = CompositeResolver::new().with(Box::new(a));
        let resolved = composite
            .resolve(&ToolName::parse("mcp/grafana/missing").unwrap())
            .unwrap();
        assert!(resolved.is_none());
    }
}
