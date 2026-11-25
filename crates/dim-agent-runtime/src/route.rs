//! Calls by id to a live owner's data face
//! ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md) D1–D4, [#2127](https://github.com/arcships/dim/issues/2127)).
//!
//! A caller outside a scope holds no instance key, so it names what it wants,
//! and the owner of the target holds the index and does the lookup:
//!
//! - a Session's [`SessionScope`] resolves one of its own lines, for the
//!   Session's parts and their runtimes; it holds no Session id, so a caller
//!   cannot name another Session;
//! - Agents' `sessions` resolves a Session, and a line of it, for the process
//!   (`dim-app-server`): it finds the Session and then calls
//!   [`SessionScope::line_call`] in that instance.
//!
//! Two data operations carry every routed call, with the same arguments in
//! every language:
//!
//! - [`SessionScope::line_call`] — `{lineId, service, method, args}`;
//! - `sessions.call` — `{sessionId, lineId?, service, method, args}`.
//!
//! An address records ids and resolves nothing; each call looks the target up
//! again, so a caller never keeps a live target (D4). Rust reads the owner's
//! face through [`line`] and [`LineHandle::get`] at the call.
//!
//! What a call resolves is the target scope's data face in the catalog. A name
//! the assembly marks `internal(reason)`, a mechanism registration point
//! (whose entries belong to the registering generation) and a scope lifecycle
//! service are refused as [`RouteError::NotDataFace`]; a data face of the
//! other scope is [`RouteError::WrongScope`]. Everything else about the call
//! belongs to the owner: the route passes the method and its arguments through
//! and returns the owner's reply, or the owner's own failure as it wrote it.
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
use std::sync::{Arc, Weak};

/// What a route answers: the target's reply, or why it did not reach one.
pub type Routed<T> = std::result::Result<T, RouteError>;

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

impl RouteError {
    /// The code a caller matches on, as the `sessions` envelope carries it.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoSession(_) => "no_session",
            Self::SessionClosing(_) => "session_closing",
            Self::NoLine { .. } => "no_line",
            Self::NotDataFace { .. } => "not_data_face",
            Self::WrongScope { .. } => "wrong_scope",
            Self::Owner { .. } => "owner",
        }
    }
}

/// A call to one of the caller's own Session's lines.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineCall {
    pub line_id: String,
    pub service: String,
    pub method: String,
    /// The method's arguments, in order.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

impl LineCall {
    /// The call to `service.method(args...)` in the line `line_id`.
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

/// A call to one of the caller's own Session's data faces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionCall {
    pub service: String,
    pub method: String,
    /// The method's arguments, in order.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

impl SessionCall {
    /// The call to `service.method(args...)` in this Session.
    pub fn new(
        service: impl Into<String>,
        method: impl Into<String>,
        args: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            service: service.into(),
            method: method.into(),
            args,
        }
    }
}

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

/// One Session's own lines and data faces, provided to the Session's parts and
/// their runtimes as the Session name `sessionScope`.
///
/// It holds its Session and no other: a part of a Session reaches its own
/// Session's lines only. `describe` joins it with base-3.
pub struct SessionScope {
    session: Weak<crate::assembly::SessionAssembly>,
    session_id: String,
}

impl SessionScope {
    /// The scope of the Session `session` is publishing.
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

    /// The live line `line_id` of this Session.
    /// The live line `line_id` of this Session. A Session that started closing
    /// while the line was being resolved answers `SessionClosing`, not
    /// `NoLine`: closing the Session closes its lines.
    fn line_context(&self, line_id: &str) -> Routed<Arc<TimelineContext>> {
        let session = self.live()?;
        session
            .line(line_id)
            .ok_or_else(|| RouteError::NoLine {
                session: self.session_id.clone(),
                line: line_id.to_owned(),
            })
            .and_then(|line| match self.live() {
                Err(closing @ RouteError::SessionClosing(_)) => Err(closing),
                _ => Ok(line),
            })
    }
}

#[rutis::service]
impl SessionScope {
    /// Calls one of this Session's own data faces.
    pub async fn call(&self, call: SessionCall) -> Routed<serde_json::Value> {
        let session = self.live()?;
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

    /// Calls one of this Session's lines' data faces.
    pub async fn line_call(&self, call: LineCall) -> Routed<serde_json::Value> {
        let line = self.line_context(&call.line_id)?;
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

/// The address of one of a Session's own lines: ids only, looked up at the
/// call ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md) D4).
pub struct LineHandle {
    scope: Arc<SessionScope>,
    line_id: String,
}

impl LineHandle {
    /// The line-scope data face `T`, resolved now; the caller uses it for this
    /// call and does not keep it.
    pub fn get<T: ?Sized + PublicService>(&self) -> Routed<Arc<T>> {
        let line = self.scope.line_context(&self.line_id)?;
        face(
            line.catalog(),
            line.faces(),
            &names::name_of::<T>(),
            BRANCH_GROUP,
        )?;
        line.public::<T>().map_err(owner)
    }
}

/// The Session-scope data face `T` of the caller's own Session, resolved now;
/// the caller uses it for this call and does not keep it.
pub fn session_face<T: ?Sized + PublicService>(scope: &SessionScope) -> Routed<Arc<T>> {
    let session = scope.live()?;
    let (catalog, faces) = session.reach();
    face(catalog, faces, &names::name_of::<T>(), SESSION_GROUP)?;
    session.session_face::<T>().map_err(owner)
}

/// Addresses line `line_id` of the caller's own Session: nothing is looked up
/// until a method of [`LineHandle`] is called.
pub fn line(scope: &Arc<SessionScope>, line_id: &str) -> LineHandle {
    LineHandle {
        scope: scope.clone(),
        line_id: line_id.to_owned(),
    }
}

/// Fails unless a route may resolve `service` as a data face of `group`
/// (module docs).
pub(crate) fn face(
    catalog: &names::ServiceCatalog,
    faces: &Faces,
    service: &str,
    group: &str,
) -> Routed<()> {
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
pub(crate) async fn call_face(
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
pub(crate) fn owner(error: impl Into<Failure>) -> RouteError {
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
