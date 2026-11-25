//! Session part: `session.tool_mode.get` and `session.tool_mode.update`
//! (agent tool modes §5). The live value belongs to ToolRegistry; the durable
//! copy is the Session's config record. An update writes the record, then
//! replaces the live value.
use crate::storage::Store;
use dim_agent_capabilities::{
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params},
    tool_registry::{ModeChangeCause, ModeTransition, ToolRegistry},
};
use dim_agent_runtime::{Names, ToolMode};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

pub(crate) const NAMESPACE: &str = "session.tool_mode";
const FAILED: &str = "tool_mode_failed";

/// The Session's two tool-mode methods; one per Session instance.
pub(crate) struct SessionToolModePlugin {
    tools: Key<ToolRegistry>,
    transition: Key<ModeTransition>,
    methods: Key<SessionMethods>,
    /// Native storage, when the assembly has it: the record carries the value.
    store: Option<Key<Store>>,
}
impl SessionToolModePlugin {
    pub(crate) fn new(names: &Names<'_>, stored: bool) -> Result<Self, Error> {
        Ok(Self {
            tools: names.key()?,
            transition: names.key()?,
            methods: names.key()?,
            store: stored.then(|| names.key()).transpose()?,
        })
    }
}
impl DynPlugin for SessionToolModePlugin {
    fn name(&self) -> &str {
        "SessionToolMode"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut injects = vec![
            self.tools.erase(),
            self.transition.erase(),
            self.methods.erase(),
        ];
        if let Some(store) = &self.store {
            injects.push(store.erase());
        }
        injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tools = ctx.require_at(&self.tools)?;
            let transition = ctx.require_at(&self.transition)?;
            let methods = ctx.require_at(&self.methods)?;
            let store = match &self.store {
                Some(key) => Some(ctx.require_at(key)?),
                None => None,
            };
            methods.register(
                ctx,
                NAMESPACE,
                vec![
                    MethodDecl::new("session.tool_mode.get", false, Interruption::Report),
                    MethodDecl::new("session.tool_mode.update", true, Interruption::Report),
                ],
                Arc::new(move |call, _cancel, _| {
                    let tools = tools.clone();
                    let transition = transition.clone();
                    let store = store.clone();
                    let MethodCall { method, params, .. } = call;
                    Box::pin(async move {
                        match method.as_str() {
                            "session.tool_mode.get" => Ok(value(&tools)),
                            _ => change(&tools, &transition, store.as_deref(), params).await,
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModeValue {
    mode: ToolMode,
    effective: ToolMode,
}

fn value(tools: &ToolRegistry) -> Value {
    serde_json::to_value(ModeValue {
        mode: tools.tool_mode(),
        effective: tools.effective_tool_mode(),
    })
    .expect("the tool mode reply is valid JSON")
}

/// `session.tool_mode.update`: writes the record, then replaces the live
/// value. A failure or a cancellation before the write leaves both unchanged;
/// once the record is written the new value stays stored, and the live value
/// follows it.
async fn change(
    tools: &ToolRegistry,
    transition: &ModeTransition,
    store: Option<&Store>,
    input: Value,
) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Params {
        session_id: String,
        mode: ToolMode,
    }
    let Params { session_id, mode } = params(input)?;
    if tools.effective_of_mode(mode) == ToolMode::Shell {
        transition
            .settle(ToolMode::Shell, ModeChangeCause::Update)
            .await
            .map_err(|error| RpcError::app(-32000, FAILED, error.to_string()))?;
    }
    if let Some(store) = store {
        store
            .set_session_tool_mode(&session_id, mode)
            .await
            .map_err(|error| RpcError::app(-32000, FAILED, error.to_string()))?;
    }
    tools.set_tool_mode(mode);
    Ok(value(tools))
}
