// Trait impls for [`BamlRuntimeManager`]: executor abstraction + `Default`.

use super::{BamlRuntimeManager, manager_prelude::*};

#[async_trait]
impl BamlFunctionExecutor for BamlRuntimeManager {
    async fn execute_function(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: Value,
    ) -> Result<Value> {
        self.invoke_function(scope, function_name, args).await
    }

    fn list_functions(&self) -> Vec<String> {
        self.state.function_registry.keys().cloned().collect()
    }
}

impl SchemaLoader for BamlRuntimeManager {
    fn load_schema(&mut self, schema_path: &str) -> Result<()> {
        self.load_schema(schema_path)
    }

    fn is_schema_loaded(&self) -> bool {
        self.is_schema_loaded()
    }
}
