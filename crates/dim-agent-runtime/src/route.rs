//! Calls by id to a live owner's data face ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md) D1–D4, [#2127](https://github.com/arcships/dim/issues/2127)).
//!
//! A caller outside a scope holds no instance key, so it names what it wants
//! instead: the [`Route`] resolves `sessionId`, or `sessionId` and a line id,
//! to the instance that is live now and reads the named owner's face there.
//! Rust calls the face directly ([`Route::session_owner`],
//! [`Route::line_owner`]); another language calls the same route by name
//! ([`Route::session`], [`Route::line`]). Neither declares the inner group's
//! names, and an instance key never leaves the instance.
//!
//! What a route resolves is the target scope's data face in the catalog: not a
//! mechanism registration point, whose entries belong to the registering
//! generation, and not a scope lifecycle service. Everything else about the
//! call belongs to the owner: the route passes the method and its arguments
//! through and returns the owner's reply or error unchanged. A target that has
//! no instance answers [`AgentError::NoSession`] (idle-unloaded, closed or
//! unknown), a Session that is closing answers [`AgentError::SessionClosing`],
//! and a line that is closing, closed or not the Session's answers
//! [`AgentError::NoLine`].

use crate::*;
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::{
    HostDispatch, Value as BridgeValue, decode_value, host_key_in, settle,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

/// Names routing does not resolve: the mechanism's registration points in the
/// groups it resolves, whose entries belong to the part that registered them
/// and are reachable only inside that generation
/// ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md)), and the
/// scope lifecycle services the runtime provides.
const REGISTRATION_POINTS: [&str; 5] = [
    "sessionMethods",
    "toolRegistry",
    "promptRegistry",
    "guideEntries",
    "submitChecks",
];

/// The call routing makes to a Session-scope data face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionFaceCall {
    pub session_id: String,
    pub service: String,
    pub method: String,
    /// The method's arguments, in order, as the calling language writes them.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

impl SessionFaceCall {
    /// The call to `service.method(args...)` in the live Session `session_id`.
    pub fn new(
        session_id: impl Into<String>,
        service: impl Into<String>,
        method: impl Into<String>,
        args: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            service: service.into(),
            method: method.into(),
            args,
        }
    }
}

/// The call routing makes to a line-scope data face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineFaceCall {
    pub session_id: String,
    pub line_id: String,
    pub service: String,
    pub method: String,
    /// The method's arguments, in order, as the calling language writes them.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

impl LineFaceCall {
    /// The call to `service.method(args...)` in the line `line_id` of the live
    /// Session `session_id`.
    pub fn new(
        session_id: impl Into<String>,
        line_id: impl Into<String>,
        service: impl Into<String>,
        method: impl Into<String>,
        args: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            line_id: line_id.into(),
            service: service.into(),
            method: method.into(),
            args,
        }
    }
}

/// Calls by id to the scope a live instance owns (module docs).
///
/// One `Route` serves every Agent of one host: each Agent registers the
/// Sessions it publishes, so a process part reaches a Session of any Agent.
/// Rust parts read it as the process name `route` and call the owner's face
/// directly; a language runtime or a UI leaf calls [`Self::session`] or
/// [`Self::line`] by that name instead.
#[derive(Default)]
pub struct Route {
    sessions: Mutex<BTreeMap<String, Weak<crate::assembly::SessionAssembly>>>,
}

impl Route {
    /// The Session-scope data face `T` of the live Session `session_id`.
    pub fn session_owner<T: ?Sized + PublicService>(&self, session_id: &str) -> Result<Arc<T>> {
        routable(&names::name_of::<T>())?;
        self.resolve_session(session_id)?.session_face::<T>()
    }

    /// The line-scope data face `T` of `line_id` in the live Session
    /// `session_id`.
    pub fn line_owner<T: ?Sized + PublicService>(
        &self,
        session_id: &str,
        line_id: &str,
    ) -> Result<Arc<T>> {
        routable(&names::name_of::<T>())?;
        self.resolve_line(session_id, line_id)?.public::<T>()
    }

    /// Registers a Session that is publishing, for as long as its instance
    /// lives.
    pub(crate) fn register(&self, id: &str, session: Weak<crate::assembly::SessionAssembly>) {
        self.sessions.lock().unwrap().insert(id.to_owned(), session);
    }

    /// Forgets `session`'s registration of `id`, unless a later Session took
    /// the id over.
    pub(crate) fn forget(&self, id: &str, session: &Arc<crate::assembly::SessionAssembly>) {
        let mut sessions = self.sessions.lock().unwrap();
        let registered = sessions
            .get(id)
            .is_some_and(|registered| Weak::ptr_eq(registered, &Arc::downgrade(session)));
        if registered {
            sessions.remove(id);
        }
    }

    fn resolve_session(&self, id: &str) -> Result<Arc<crate::assembly::SessionAssembly>> {
        let session = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| AgentError::NoSession(id.into()))?;
        if session.closing.is_cancelled() {
            return Err(AgentError::SessionClosing(id.into()));
        }
        Ok(session)
    }

    fn resolve_line(&self, session_id: &str, line_id: &str) -> Result<Arc<TimelineContext>> {
        let session = self.resolve_session(session_id)?;
        session.line(line_id).ok_or_else(|| AgentError::NoLine {
            session: session_id.to_owned(),
            line: line_id.to_owned(),
        })
    }
}

#[rutis::service]
impl Route {
    /// Calls a Session-scope data face: [`Route::session_owner`] for a
    /// language that names the face instead of holding it.
    pub async fn session(&self, call: SessionFaceCall) -> Result<serde_json::Value> {
        routable(&call.service)?;
        let session = self.resolve_session(&call.session_id)?;
        let loaded = session.loaded()?;
        call_face(
            &loaded.ctx,
            loaded.scope.instance,
            &call.service,
            &call.method,
            &call.args,
        )
        .await
    }

    /// Calls a line-scope data face: [`Route::line_owner`] for a language that
    /// names the face instead of holding it.
    pub async fn line(&self, call: LineFaceCall) -> Result<serde_json::Value> {
        routable(&call.service)?;
        let line = self.resolve_line(&call.session_id, &call.line_id)?;
        call_face(
            line.ctx(),
            line.scope_instance(),
            &call.service,
            &call.method,
            &call.args,
        )
        .await
    }
}

/// Fails unless routing may resolve `service` (module docs).
fn routable(service: &str) -> Result<()> {
    let lifecycle = [
        names::name_of::<SessionLifecycle>(),
        names::name_of::<BranchLifecycle>(),
    ];
    if REGISTRATION_POINTS.contains(&service) || lifecycle.iter().any(|name| name == service) {
        return Err(AgentError::InvalidInput(format!(
            "route: {service} is not a routable service"
        )));
    }
    Ok(())
}

/// Calls `method` of the data face `service` in `instance`, as another
/// language would: the arguments travel as data and the reply comes back as
/// data.
async fn call_face(
    ctx: &Ctx,
    instance: rutis::host::InstanceId,
    service: &str,
    method: &str,
    args: &[serde_json::Value],
) -> Result<serde_json::Value> {
    let dispatch = ctx
        .get_at::<dyn HostDispatch>(&host_key_in(service, instance))
        .ok_or_else(|| AgentError::Reloading(service.into()))?;
    let args = BridgeValue::List(
        args.iter()
            .map(|arg| BridgeValue::Data(arg.clone()))
            .collect(),
    );
    let reply = dispatch
        .invoke(method, args)
        .map_err(|error| AgentError::fault(service, error.to_string()))?;
    let value = settle(reply)
        .await
        .map_err(|error| AgentError::fault(service, error.to_string()))?;
    decode_value::<serde_json::Value>(value)
        .map_err(|error| AgentError::fault(service, error.to_string()))
}

/// Provides the host's [`Route`] at the process name `route`.
pub(crate) struct RoutePlugin {
    pub route: Arc<Route>,
    pub key: rutis::host::Key<Route>,
}

impl rutis::Plugin for RoutePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "Route"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide_at(self.key.clone(), self.route.clone())?;
        Ok(())
    }
}
