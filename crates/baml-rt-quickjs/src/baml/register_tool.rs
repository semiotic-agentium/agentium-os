// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use super::{BamlRuntimeManager, manager_prelude::*};

impl BamlRuntimeManager {
    /// Register a tool that implements the BamlTool trait
    ///
    /// Tools can be called by LLMs during BAML function execution
    /// or directly from JavaScript via the QuickJS bridge.
    ///
    /// # Example
    /// ```rust,no_run
    /// use baml_rt::baml::BamlRuntimeManager;
    /// use baml_rt::tools::BamlTool;
    /// use baml_rt_tools::DescribeAction;
    /// use baml_rt_tools::bundles::Support;
    /// use async_trait::async_trait;
    /// use baml_derive::BamlType;
    /// use serde::{Deserialize, Serialize};
    ///
    /// struct MyTool;
    ///
    /// #[derive(Serialize, Deserialize, BamlType)]
    /// struct MyInput {}
    ///
    /// impl DescribeAction for MyInput {
    ///     fn describe(&self) -> String {
    ///         "run my_tool".to_string()
    ///     }
    /// }
    ///
    /// #[derive(Serialize, Deserialize, BamlType)]
    /// struct MyOutput {
    ///     result: String,
    /// }
    ///
    /// #[async_trait]
    /// impl BamlTool for MyTool {
    ///     type Bundle = Support;
    ///     const LOCAL_NAME: &'static str = "my_tool";
    ///     type OpenInput = ();
    ///     type Input = MyInput;
    ///     type Output = MyOutput;
    ///     fn description(&self) -> &'static str { "Does something" }
    ///     async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
    ///         Ok(MyOutput { result: "success".to_string() })
    ///     }
    /// }
    ///
    /// # tokio_test::block_on(async {
    /// let mut manager = BamlRuntimeManager::builder().build()?;
    /// manager.register_tool(MyTool).await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    pub async fn register_tool<T: baml_rt_tools::BamlTool>(&mut self, tool: T) -> Result<()> {
        self.state.tool_registry.register(tool)
    }
}
