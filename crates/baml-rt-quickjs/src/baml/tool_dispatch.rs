use tool_invocation_plan::BamlToolInvocationPlan;

use super::{BamlRuntimeManager, manager_prelude::*, tool_invocation_plan};

impl BamlRuntimeManager {
    /// Execute a tool from a BAML result
    ///
    /// BAML returns either:
    /// - A `ToolSessionPlan` describing FSM steps, or
    /// - A `tool_name` payload for a one-shot session.
    ///
    /// The runtime executes host tools via the session FSM in Rust.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use baml_rt::baml::BamlRuntimeManager;
    /// # use baml_rt::tools::BamlTool;
    /// # use baml_rt_tools::bundles::Support;
    /// # use async_trait::async_trait;
    /// # use baml_derive::BamlType;
    /// # use serde::{Deserialize, Serialize};
    /// # struct WeatherTool;
    /// # #[derive(Serialize, Deserialize, BamlType)]
    /// # struct WeatherInput { location: String }
    /// # impl baml_rt_tools::DescribeAction for WeatherInput {
    /// #     fn describe(&self) -> String { format!("weather for {}", self.location) }
    /// # }
    /// # #[derive(Serialize, Deserialize, BamlType)]
    /// # struct WeatherOutput { temperature: String }
    /// # #[async_trait]
    /// # impl BamlTool for WeatherTool {
    /// #     type Bundle = Support;
    /// #     const LOCAL_NAME: &'static str = "get_weather";
    /// #     type OpenInput = ();
    /// #     type Input = WeatherInput;
    /// #     type Output = WeatherOutput;
    /// #     fn description(&self) -> &'static str { "" }
    /// #     async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
    /// #         Ok(WeatherOutput { temperature: "22°C".to_string() })
    /// #     }
    /// # }
    /// # tokio_test::block_on(async {
    /// # let mut manager = BamlRuntimeManager::builder().build()?;
    /// manager.register_tool(WeatherTool).await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    /// Execute a tool from a BAML union type result
    ///
    /// Takes a BAML result (typed class or single-key object),
    /// derives the tool from the type name, and executes it.
    ///
    /// # Arguments
    /// * `baml_result` - The JSON result from BAML function (union variant)
    ///
    /// # Returns
    /// The result of executing the tool function
    pub async fn execute_tool_from_baml_result(
        &self,
        scope: &context::RuntimeScope,
        baml_result: Value,
    ) -> Result<Value> {
        let call = extract_tool_call(&baml_result)?.ok_or_else(|| {
            BamlRtError::InvalidArgument("No tool call found in result".to_string())
        })?;
        let tool_name = self.resolve_tool_name_from_input(&call.args).await?;
        self.execute_tool(scope, &tool_name.to_string(), call.args)
            .await
    }

    /// Execute a tool from a BAML result: session plan (requires source_baml_function) or single tool call (resolved by input schema).
    ///
    /// Session plans are bound to a tool by manifest mapping (function name -> plan type).
    /// Runtime requires the invoking function to be present in the builder-generated
    /// `session_plan_functions.json` so tool resolution does not rely on prompt-emitted `__type`.
    pub async fn execute_tool_from_baml_result_or_value(
        &self,
        scope: &context::RuntimeScope,
        baml_result: Value,
        source_baml_function: Option<&str>,
        invocation_args: Option<&Value>,
    ) -> Result<Value> {
        tracing::debug!(
            baml_result = %baml_result,
            source_function = ?source_baml_function,
            "execute_tool_from_baml_result_or_value: entry"
        );
        let handle = self.tool_session_handle();
        let classified = tool_invocation_plan::resolve_baml_tool_invocation_plan(
            scope,
            &handle,
            baml_result,
            source_baml_function,
            invocation_args,
            &self.state.session_plan_functions,
            &self.state.tool_registry,
        )?;
        let plan_kind = match &classified {
            BamlToolInvocationPlan::Passthrough(_) => "passthrough",
            BamlToolInvocationPlan::ArchiveRead { .. } => "archive_read",
            BamlToolInvocationPlan::SessionPlan { .. } => "session_plan",
            BamlToolInvocationPlan::OneShot { .. } => "one_shot",
        };
        tracing::debug!(
            plan_kind,
            "execute_tool_from_baml_result_or_value: classified invocation plan"
        );
        match classified {
            BamlToolInvocationPlan::ArchiveRead { plan } => {
                self.execute_archive_read_plan(scope, plan).await
            }
            BamlToolInvocationPlan::SessionPlan {
                tool_name,
                plan,
                source_baml_function: src,
                invocation_args: inv,
            } => {
                self.execute_tool_session_plan(scope, tool_name, plan, src.as_deref(), inv.as_ref())
                    .await
            }
            BamlToolInvocationPlan::OneShot { tool_name, args } => {
                self.execute_tool(scope, &tool_name.to_string(), args).await
            }
            BamlToolInvocationPlan::Passthrough(v) => Ok(v),
        }
    }

    async fn resolve_tool_name_from_input(&self, input: &Value) -> Result<baml_rt_tools::ToolName> {
        resolve_tool_name_from_input_with_registry(&self.state.tool_registry, input)
    }
}
