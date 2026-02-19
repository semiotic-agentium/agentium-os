use std::collections::HashMap;

use baml_rt_core::{
    BamlRtError, Result,
    ids::{ContextId, TaskId},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

use crate::a2a_types::{
    GetTaskRequest, JSONRPCId, JSONRPCRequest, ListTasksRequest, NumberOrString,
    SendMessageRequest, SubscribeToTaskRequest, TaskState,
};

/// Strictly-typed JSON-RPC request envelope for A2A.
#[derive(Debug, Clone)]
pub struct A2aWireRequest {
    pub id: Option<JSONRPCId>,
    pub method: A2aWireMethod,
}

impl A2aWireRequest {
    pub fn try_from_raw(raw: JSONRPCRequest) -> Result<Self> {
        let params = raw.params.unwrap_or(Value::Null);
        let method = match raw.method.as_str() {
            "message.sendStream" => A2aWireMethod::MessageSendStream {
                params: Box::new(parse_params(params)?),
            },
            "message.send" => {
                return Err(BamlRtError::InvalidArgument(
                    "Only message.sendStream is supported".to_string(),
                ));
            }
            "tasks.get" => A2aWireMethod::TasksGet {
                params: parse_params(params)?,
            },
            "tasks.list" => A2aWireMethod::TasksList {
                params: parse_params_or_default(params)?,
            },
            "tasks.subscribe" => A2aWireMethod::TasksSubscribe {
                params: parse_params(params)?,
            },
            _ => {
                return Err(BamlRtError::InvalidArgument(
                    "Unsupported A2A request method".to_string(),
                ));
            }
        };

        Ok(Self { id: raw.id, method })
    }
}

fn parse_params<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(BamlRtError::Json)
}

fn parse_params_or_default<T: Default + DeserializeOwned>(value: Value) -> Result<T> {
    if value.is_null() {
        Ok(T::default())
    } else {
        parse_params(value)
    }
}

#[derive(Debug, Clone)]
pub enum A2aWireMethod {
    MessageSendStream { params: Box<SendMessageRequest> },
    TasksGet { params: GetTaskRequest },
    TasksList { params: ListTasksParams },
    TasksSubscribe { params: SubscribeToTaskParams },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Wire-only params for tasks.list.
///
/// This mirrors ListTasksRequest but derives Default so we can accept omitted
/// params (null or missing) on the wire and treat them as empty.
pub struct ListTasksParams {
    pub context_id: Option<ContextId>,
    pub history_length: Option<NumberOrString>,
    pub include_artifacts: Option<bool>,
    pub page_size: Option<NumberOrString>,
    pub page_token: Option<String>,
    pub status: Option<TaskState>,
    pub status_timestamp_after: Option<String>,
    pub tenant: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl From<ListTasksParams> for ListTasksRequest {
    fn from(params: ListTasksParams) -> Self {
        Self {
            context_id: params.context_id,
            history_length: params.history_length,
            include_artifacts: params.include_artifacts,
            page_size: params.page_size,
            page_token: params.page_token,
            status: params.status,
            status_timestamp_after: params.status_timestamp_after,
            tenant: params.tenant,
            extra: params.extra,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeToTaskParams {
    pub id: TaskId,
    pub tenant: Option<String>,
    pub stream: Option<bool>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl From<SubscribeToTaskParams> for SubscribeToTaskRequest {
    fn from(params: SubscribeToTaskParams) -> Self {
        // NOTE: `stream` is handled by the transport layer; invocation kind is inferred separately.
        Self {
            id: params.id,
            tenant: params.tenant,
            extra: params.extra,
        }
    }
}
