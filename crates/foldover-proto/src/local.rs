use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod pair;
pub use pair::{
    PAIR_CREATE, PAIR_GET, PAIR_LIST, PAIR_PAGE_SIZE, PAIR_REMOVE, PAIR_UPDATE, PairInput, PairKey,
    PairList, PairMode, PairPage, PairRecord, PairRevision, PairSide, PairUpdate,
};

pub const JSON_RPC_VERSION: &str = "2.0";
pub const PROTOCOL_VERSION: u32 = 2;
pub const SESSION_HELLO: &str = "session.hello";
pub const DAEMON_STATUS: &str = "daemon.status";
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
pub const VERSION_MISMATCH: i32 = -32001;
pub const PERMISSION_DENIED: i32 = -32002;
pub const PAIR_NOT_FOUND: i32 = -32003;
pub const STALE_REVISION: i32 = -32004;
pub const ROOT_UNAVAILABLE: i32 = -32005;
pub const ROOT_OVERLAP: i32 = -32006;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub core_version: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentCapability {
    HealthOnly,
    PairConfiguration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DaemonStatus {
    pub core_version: String,
    pub capability: AgentCapability,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RpcError {
    code: i32,
    message: String,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> i32 {
        self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

impl Request {
    pub fn call(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            id: Some(id.into()),
            method: method.into(),
            params,
        }
    }

    /// Keep missing IDs distinct from explicit null IDs: only missing means notification.
    pub fn parse(value: Value) -> Result<Self, Response> {
        let invalid = || {
            Response::error(
                Value::Null,
                RpcError::new(INVALID_REQUEST, "Invalid request"),
            )
        };
        let object = value.as_object().ok_or_else(invalid)?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
            return Err(invalid());
        }
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let id = object.get("id").cloned();
        if id.as_ref().is_some_and(|id| !valid_id(id)) {
            return Err(invalid());
        }
        Ok(Self {
            id,
            method: method.to_owned(),
            params: object.get("params").cloned(),
        })
    }

    pub fn id(&self) -> Option<&Value> {
        self.id.as_ref()
    }
    pub fn method(&self) -> &str {
        &self.method
    }
    pub fn params(&self) -> Option<&Value> {
        self.params.as_ref()
    }

    pub fn to_value(&self) -> Value {
        let mut value = json!({"jsonrpc": JSON_RPC_VERSION, "method": self.method});
        if let Some(id) = &self.id {
            value["id"] = id.clone();
        }
        if let Some(params) = &self.params {
            value["params"] = params.clone();
        }
        value
    }
}

fn valid_id(id: &Value) -> bool {
    id.is_null() || id.is_string() || id.is_number()
}

#[derive(Clone, Debug)]
pub struct Response {
    id: Value,
    result: Result<Value, RpcError>,
}

impl Response {
    pub fn result(id: Value, result: Value) -> Self {
        Self {
            id,
            result: Ok(result),
        }
    }
    pub fn error(id: Value, error: RpcError) -> Self {
        Self {
            id,
            result: Err(error),
        }
    }
    pub fn id(&self) -> &Value {
        &self.id
    }
    pub fn into_result(self) -> Result<Value, RpcError> {
        self.result
    }

    pub fn to_value(&self) -> Value {
        match &self.result {
            Ok(result) => json!({"jsonrpc": JSON_RPC_VERSION, "id": self.id, "result": result}),
            Err(error) => json!({"jsonrpc": JSON_RPC_VERSION, "id": self.id, "error": error}),
        }
    }

    pub fn parse(value: Value) -> Result<Self, &'static str> {
        let object = value.as_object().ok_or("Response is not an object")?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
            return Err("Unsupported JSON-RPC response version");
        }
        let id = object
            .get("id")
            .filter(|id| valid_id(id))
            .ok_or("Invalid response ID")?
            .clone();
        match (object.get("result"), object.get("error")) {
            (Some(result), None) => Ok(Self::result(id, result.clone())),
            (None, Some(error)) => {
                let error =
                    serde_json::from_value(error.clone()).map_err(|_| "Invalid RPC error")?;
                Ok(Self::error(id, error))
            }
            _ => Err("A response needs exactly one result or error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_notifications_from_null_ids() {
        let notification =
            Request::parse(json!({"jsonrpc":"2.0", "method":"daemon.status"})).unwrap();
        let explicit_null =
            Request::parse(json!({"jsonrpc":"2.0", "method":"daemon.status", "id":null})).unwrap();
        assert!(notification.id().is_none());
        assert_eq!(explicit_null.id(), Some(&Value::Null));
    }

    #[test]
    fn rejects_invalid_envelopes_and_ambiguous_responses() {
        for value in [
            json!([]),
            json!({"jsonrpc":"1.0","method":"x","id":1}),
            json!({"jsonrpc":"2.0","method":4,"id":1}),
            json!({"jsonrpc":"2.0","method":"x","id":true}),
        ] {
            assert!(Request::parse(value).is_err());
        }
        assert!(
            Response::parse(
                json!({"jsonrpc":"2.0","id":1,"result":null,"error":{"code":-1,"message":"x"}})
            )
            .is_err()
        );
        assert!(Response::parse(json!({"jsonrpc":"2.0","result":null})).is_err());
    }

    #[test]
    fn preserves_numeric_string_and_null_ids() {
        for id in [
            json!(1),
            json!(-2),
            json!(1.5),
            json!("client"),
            Value::Null,
        ] {
            let request = Request::parse(json!({"jsonrpc":"2.0","method":"x","id":id})).unwrap();
            let response = Response::result(request.id().unwrap().clone(), json!({"ok":true}));
            assert_eq!(Response::parse(response.to_value()).unwrap().id(), &id);
        }
    }
}
