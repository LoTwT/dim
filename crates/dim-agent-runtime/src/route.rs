//! Calls by id to a live owner's data face
//! ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md) D1–D4, [#2127](https://github.com/arcships/dim/issues/2127)).
//!
//! A caller outside a scope holds no instance key, so it names what it wants.
//! The runtime keeps the Sessions its Agents publish and the lines each
//! Session created, and hands the call to the instance that is live now:
//!
//! - a process caller names the Session, or the Session and a line
//!   ([`Route`]);
//! - a Session part names one of its own lines only ([`LineRoute`]); it cannot
//!   name another Session.
//!
//! A Rust caller calls the target owner's operations directly: [`line_owner`]
//! is the binding of [`LineRoute::line`] for a caller that wants the owner's
//! face. Everything else in this module is data: Rust, another language's
//! runtime and a UI leaf call the same operation with the same arguments and
//! read the same reply. Neither route takes a target's instance key, and a
//! resolution is never kept across calls.
//!
//! What a route resolves is the target scope's data face in the catalog. A
//! name the assembly marks `internal(reason)`, a mechanism registration point
//! (whose entries belong to the registering generation) and a scope lifecycle
//! service are refused as [`RouteError::NotDataFace`]; a name of another
//! scope is [`RouteError::WrongScope`]. Everything else about the call belongs
//! to the owner: the route passes the method and its arguments through and
//! returns the owner's reply, or the owner's own failure as it wrote it.
//!
//! A Session that has no live instance — idle-unloaded, closed or never
//! created — answers [`RouteError::NoSession`]. A Session that is closing
//! answers [`RouteError::SessionClosing`], and a line target answers it too,
//! because closing the Session closes its lines. A line that is closing or
//! closed, or that belongs to another Session, answers [`RouteError::NoLine`];
//! that is final for the line: routing never retries it.

use crate::*;
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Value as BridgeValue, decode_value, host_key_in, settle,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

/// What a route answers: the target's reply, or why it did not reach one.
type Routed<T> = std::result::Result<T, RouteError>;

/// Names routing does not resolve: the mechanism's registration points, whose
/// entries belong to the part that registered them and are reachable only
/// inside that generation
/// ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md) D3), and the
/// scope lifecycle services the runtime provides.
const REGISTRATION_POINTS: [&str; 5] = [
    "sessionMethods",
    "toolRegistry",
    "promptRegistry",
    "guideEntries",
    "submitChecks",
];

/// Why a routed call did not reach a reply.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// The Session has no live instance: it was idle-unloaded, closed, or
    /// never created. Only a process caller receives it; a caller that must
    /// continue asks Agents for a hold and calls again once that loads the
    /// Session.
    #[error("no live Session {0}")]
    NoSession(String),
    /// The Session instance is closing, and so is a call to one of its lines.
    /// A Session-scoped caller ends its work; a process caller treats it as
    /// [`Self::NoSession`].
    #[error("Session {0} is closing")]
    SessionClosing(String),
    /// The Session is live but the line is closing, closed, or not one of its
    /// lines. Final for that line: no hold and no retry.
    #[error("no line {line} in Session {session}")]
    NoLine { session: String, line: String },
    /// The name is not a data face a route resolves there: the assembly marks
    /// it internal, it is a mechanism registration point, or it is a scope
    /// lifecycle service. A permanent answer: the request is wrong.
    #[error("{service} is not a routable data face")]
    NotDataFace { service: String },
    /// The name is a data face, but not of the scope the route reaches.
    #[error("{service} is not a {group} service")]
    WrongScope { service: String, group: String },
    /// The owner's own failure, with the name and message it wrote; routing
    /// does not read or restate it.
    #[error("{message}")]
    Owner { name: String, message: String },
}

/// A call to a Session-scope data face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionFaceCall {
    pub session_id: String,
    pub service: String,
    pub method: String,
    /// The method's arguments, in order.
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

/// A call from the process to a line-scope data face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineFaceCall {
    pub session_id: String,
    pub line_id: String,
    pub service: String,
    pub method: String,
    /// The method's arguments, in order.
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

/// A call from a Session part to one of its own lines.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnLineCall {
    pub line_id: String,
    pub service: String,
    pub method: String,
    /// The method's arguments, in order.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

impl OwnLineCall {
    /// The call to `service.method(args...)` in this Session's line `line_id`.
    pub fn new(
        line_id: impl Into<String>,
        service: impl Into<String>,
        method: impl Into<String>,
        args: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            line_id: line_id.into(),
            service: service.into(),
            method: method.into(),
            args,
        }
    }
}

/// The Sessions this host's Agents published, by id, and the lines each
/// Session created. Runtime state, not a service: the routes read it, and a
/// Session registers itself as it publishes.
#[derive(Default)]
pub(crate) struct Index {
    sessions: Mutex<BTreeMap<String, Weak<crate::assembly::SessionAssembly>>>,
}

impl Index {
    /// Records the Session publishing as `id`.
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

    fn session(&self, id: &str) -> Routed<Arc<crate::assembly::SessionAssembly>> {
        let session = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| RouteError::NoSession(id.into()))?;
        if session.closing.is_cancelled() {
            return Err(RouteError::SessionClosing(id.into()));
        }
        Ok(session)
    }

    fn line(&self, session_id: &str, line_id: &str) -> Routed<Arc<TimelineContext>> {
        let session = self.session(session_id)?;
        session.line(line_id).ok_or_else(|| RouteError::NoLine {
            session: session_id.to_owned(),
            line: line_id.to_owned(),
        })
    }
}

/// Calls by id to a live Session or line (process scope), provided as the
/// process name `route`.
///
/// One route serves every Agent of one host: each Agent registers the Sessions
/// it publishes, so a process part reaches a Session of any of them.
pub struct Route {
    index: Arc<Index>,
}

impl Route {
    /// The route over `index`.
    pub(crate) fn new(index: Arc<Index>) -> Self {
        Self { index }
    }
}

#[rutis::service]
impl Route {
    /// Calls a Session-scope data face of the live Session `call.session_id`.
    pub async fn session(&self, call: SessionFaceCall) -> Routed<serde_json::Value> {
        let session = self.index.session(&call.session_id)?;
        let (catalog, faces) = session.reach();
        face(catalog, faces, &call.service, SESSION_GROUP)?;
        let loaded = session.loaded().map_err(owner)?;
        call_face(
            &loaded.ctx,
            loaded.scope.instance,
            &call.service,
            &call.method,
            &call.args,
        )
        .await
    }

    /// Calls a line-scope data face of the line `call.line_id` in the live
    /// Session `call.session_id`.
    pub async fn line(&self, call: LineFaceCall) -> Routed<serde_json::Value> {
        let line = self.index.line(&call.session_id, &call.line_id)?;
        face(line.catalog(), line.faces(), &call.service, BRANCH_GROUP)?;
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

/// Calls by id to one of this Session's lines, provided to the Session's parts
/// and their runtimes as the Session name `lineRoute`.
///
/// A Session part reaches its own Session's lines only: the route holds no
/// Session id, so a caller cannot name another Session, and a line of another
/// Session is [`RouteError::NoLine`].
pub struct LineRoute {
    session: Weak<crate::assembly::SessionAssembly>,
    session_id: String,
}

impl LineRoute {
    /// The route over the Session `session` is publishing.
    pub(crate) fn new(session: &Arc<crate::assembly::SessionAssembly>) -> Self {
        Self {
            session: Arc::downgrade(session),
            session_id: session.id.clone(),
        }
    }

    fn live(&self) -> Routed<Arc<crate::assembly::SessionAssembly>> {
        let closing = || RouteError::SessionClosing(self.session_id.clone());
        let session = self.session.upgrade().ok_or_else(closing)?;
        if session.closing.is_cancelled() {
            return Err(closing());
        }
        Ok(session)
    }

    fn live_line(&self, line_id: &str) -> Routed<Arc<TimelineContext>> {
        self.live()?
            .line(line_id)
            .ok_or_else(|| RouteError::NoLine {
                session: self.session_id.clone(),
                line: line_id.to_owned(),
            })
    }
}

#[rutis::service]
impl LineRoute {
    /// Calls a line-scope data face of this Session's line `call.line_id`.
    pub async fn line(&self, call: OwnLineCall) -> Routed<serde_json::Value> {
        let line = self.live_line(&call.line_id)?;
        face(line.catalog(), line.faces(), &call.service, BRANCH_GROUP)?;
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

/// The line-scope data face `T` of this Session's line `line_id`: the Rust
/// binding of [`LineRoute::line`].
pub fn line_owner<T: ?Sized + PublicService>(route: &LineRoute, line_id: &str) -> Routed<Arc<T>> {
    let line = route.live_line(line_id)?;
    face(
        line.catalog(),
        line.faces(),
        &names::name_of::<T>(),
        BRANCH_GROUP,
    )?;
    line.public::<T>().map_err(owner)
}

/// Fails unless a route may resolve `service` as a data face of `group`
/// (module docs).
fn face(catalog: &names::ServiceCatalog, faces: &Faces, service: &str, group: &str) -> Routed<()> {
    let lifecycle = [
        names::name_of::<SessionLifecycle>(),
        names::name_of::<BranchLifecycle>(),
    ];
    if REGISTRATION_POINTS.contains(&service)
        || lifecycle.iter().any(|name| name == service)
        || faces.reason(service).is_some()
    {
        return Err(RouteError::NotDataFace {
            service: service.into(),
        });
    }
    names::named_in_group(catalog, service, group).map_err(|_| RouteError::WrongScope {
        service: service.into(),
        group: group.into(),
    })
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
) -> Routed<serde_json::Value> {
    let dispatch = ctx
        .get_at::<dyn HostDispatch>(&host_key_in(service, instance))
        .ok_or_else(|| RouteError::Owner {
            name: service.into(),
            message: format!("{service} is not provided now"),
        })?;
    let args = BridgeValue::List(
        args.iter()
            .map(|arg| BridgeValue::Data(arg.clone()))
            .collect(),
    );
    let reply = dispatch.invoke(method, args).map_err(owner)?;
    let value = settle(reply).await.map_err(owner)?;
    decode_value::<serde_json::Value>(value).map_err(owner)
}

/// The owner's failure as the target wrote it, under the name it carries.
fn owner(error: impl Into<Failure>) -> RouteError {
    let failure = error.into();
    RouteError::Owner {
        name: failure.name,
        message: failure.message,
    }
}

/// A failure's name and message, from either error surface a read answers
/// with.
pub(crate) struct Failure {
    name: String,
    message: String,
}

impl From<AgentError> for Failure {
    fn from(error: AgentError) -> Self {
        Self {
            name: "AgentError".into(),
            message: error.to_string(),
        }
    }
}

impl From<BridgeError> for Failure {
    fn from(error: BridgeError) -> Self {
        match error {
            BridgeError::Remote { name, message, .. } => Self { name, message },
            BridgeError::Binding { code, message, .. } => Self {
                name: code.unwrap_or("BindingError").into(),
                message,
            },
            other => Self {
                name: "BridgeError".into(),
                message: other.to_string(),
            },
        }
    }
}

/// Provides the host's [`Route`] at the process name `route`.
pub(crate) struct RoutePlugin {
    pub index: Arc<Index>,
    pub key: rutis::host::Key<Route>,
}

impl rutis::Plugin for RoutePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "Route"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide_at(self.key.clone(), Arc::new(Route::new(self.index.clone())))?;
        Ok(())
    }
}
