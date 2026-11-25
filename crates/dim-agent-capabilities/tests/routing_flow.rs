//! Flow: a call by id reaches the data face of a live Session or line, and
//! answers `NoSession`, `SessionClosing`, `NoLine` or a reach refusal
//! otherwise ([#2127](https://github.com/arcships/dim/issues/2127)).

use dim_agent_capabilities::{defaults, timeline_tree::BranchLine};
use dim_agent_runtime::testing::{MinimalHost, hold_close};
use dim_agent_runtime::*;
use dim_agent_types::BranchSource;
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, InstanceId, Key, TypeKey};
use rutis_bridge::session::{
    Connection, Dispatch, Endpoint, Error as BridgeError, HostDispatch, Reply, Value, host_key,
    host_key_in,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A Session part that hands its context to the test, where a language's
/// Session runtime would run.
struct HandsSession(Arc<Mutex<Option<Ctx>>>);

impl DynPlugin for HandsSession {
    fn name(&self) -> &str {
        "RoutingSessionProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> rutis::host::BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(ctx.clone());
            Ok(Effect::Done)
        })
    }
}

struct Rig {
    host: MinimalHost,
    route: Arc<Route>,
    /// The context of the last Session's part, once that Session opened.
    session_ctx: Arc<Mutex<Option<Ctx>>>,
}

impl Rig {
    async fn start() -> Self {
        let session_ctx = Arc::new(Mutex::new(None));
        let probe = session_ctx.clone();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )));
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        // The product marks the same names (its `register_faces`); a route
        // resolves data faces only.
        builder.internal_name("loopState", "hot-path");
        builder.require_feature(
            Feature::new("RoutingSessionProbe").session("RoutingSessionProbe", move |_| {
                Ok(Box::new(HandsSession(probe.clone())))
            }),
        );
        let host = MinimalHost::start(&builder).await.unwrap();
        let route = host
            .root()
            .get_at::<Route>(&Key::<Route>::of())
            .expect("the host serves the route");
        Self {
            host,
            route,
            session_ctx,
        }
    }

    async fn open(&self) -> Session {
        self.host
            .create_session(SessionConfig::default())
            .await
            .expect("a Session opens")
    }

    fn session_ctx(&self) -> Ctx {
        self.session_ctx
            .lock()
            .unwrap()
            .take()
            .expect("the Session's part loaded")
    }

    fn line_route(&self, session: &Session) -> Arc<LineRoute> {
        session
            .session_service::<LineRoute>()
            .expect("the Session serves its line route")
    }
}

/// A language runtime's side of a rutis session: it names a service in `ctx`'s
/// subtree, and the host serves it.
struct Names {
    ctx: Ctx,
    /// The instance the runtime lives in: its names are served there first,
    /// then at the process.
    instance: InstanceId,
}

impl Names {
    /// The dispatch `target` names for this runtime, as a row of another
    /// language resolves one.
    fn dispatch(&self, target: &str) -> Arc<dyn HostDispatch> {
        self.ctx
            .get_at::<dyn HostDispatch>(&host_key_in(target, self.instance))
            .or_else(|| self.ctx.get_at::<dyn HostDispatch>(&host_key(target)))
            .unwrap_or_else(|| panic!("{target} is not served"))
    }
}

impl Dispatch for Names {
    fn invoke(&self, _: &Connection, target: &str, method: &str, args: Value) -> Reply {
        self.dispatch(target).invoke(method, args)
    }
}

/// Both ends of a rutis session with the tree of `ctx`.
struct Language {
    _host: Connection,
    client: Connection,
}

impl Language {
    fn open(ctx: &Ctx, instance: InstanceId) -> Self {
        let (host, far) = rutis_bridge::memory::pair();
        let peer = |id: &str| Endpoint::rust(rutis_bridge::channel::PeerId::new(id).unwrap());
        let names = || Names {
            ctx: ctx.clone(),
            instance,
        };
        let host = Connection::open(host, Arc::new(names()), peer("route-host")).unwrap();
        let client = Connection::open(far, Arc::new(names()), peer("route-client")).unwrap();
        Self {
            _host: host,
            client,
        }
    }

    async fn call(
        &self,
        face: &str,
        method: &str,
        call: impl serde::Serialize,
    ) -> std::result::Result<serde_json::Value, BridgeError> {
        let call: Value = serde_json::to_value(call).unwrap().into();
        let reply = self
            .client
            .invoke_settled(face, method, Value::List(vec![call]))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), reply)
            .await
            .expect("the far end answers")?
            .json()
    }

    async fn refused(&self, face: &str, method: &str, call: impl serde::Serialize) -> String {
        self.call(face, method, call)
            .await
            .expect_err("the route refuses the call")
            .to_string()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_call_reaches_the_live_session_and_line() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();

    let served = rig
        .route
        .session(SessionFaceCall::new(&id, "sessionConfig", "get", vec![]))
        .await
        .unwrap();
    assert_eq!(served, serde_json::to_value(session.config()).unwrap());
    let sequence = rig
        .route
        .line(LineFaceCall::new(
            &id,
            "main",
            "timeline",
            "sequence",
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(
        sequence,
        serde_json::json!(session.sequence("main").unwrap())
    );

    assert_eq!(
        rig.route
            .session(SessionFaceCall::new(
                "absent",
                "sessionConfig",
                "get",
                vec![]
            ))
            .await
            .unwrap_err(),
        RouteError::NoSession("absent".into())
    );
    assert_eq!(
        rig.route
            .line(LineFaceCall::new(
                &id,
                "absent",
                "timeline",
                "sequence",
                vec![]
            ))
            .await
            .unwrap_err(),
        RouteError::NoLine {
            session: id.clone(),
            line: "absent".into()
        }
    );
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_part_reaches_its_own_line_by_id_only() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let route = rig.line_route(&session);

    let sequence = route
        .line(OwnLineCall::new("main", "timeline", "sequence", vec![]))
        .await
        .unwrap();
    assert_eq!(
        sequence,
        serde_json::json!(session.sequence("main").unwrap())
    );
    assert_eq!(
        route
            .line(OwnLineCall::new("absent", "timeline", "sequence", vec![]))
            .await
            .unwrap_err(),
        RouteError::NoLine {
            session: id.clone(),
            line: "absent".into()
        }
    );
    assert!(line_owner::<BranchLine>(&route, "main").is_ok());
    assert!(
        dim_agent_runtime::line(&route, "main")
            .owner::<BranchLine>()
            .is_ok(),
        "the address form resolves at the call"
    );
    assert_eq!(
        line_owner::<dyn LoopState>(&route, "main").err(),
        Some(RouteError::NotDataFace {
            service: "loopState".into()
        }),
        "the Rust binding resolves data faces only"
    );
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_language_call_reaches_a_line_face_through_the_route() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let language = Language::open(rig.host.root(), rig.host.root().instance());
    language.client.ready().await.unwrap();

    let sequence = language
        .call(
            "route",
            "line",
            LineFaceCall::new(&id, "main", "timeline", "sequence", vec![]),
        )
        .await
        .unwrap();
    assert_eq!(
        sequence,
        serde_json::json!(session.sequence("main").unwrap())
    );
    let error = language
        .refused(
            "route",
            "line",
            LineFaceCall::new(&id, "absent", "timeline", "sequence", vec![]),
        )
        .await;
    assert!(error.contains("no line"), "{error}");
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_language_call_reaches_its_own_line_and_names_no_session() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let language = Language::open(&rig.session_ctx(), session.instance().unwrap());
    language.client.ready().await.unwrap();

    let sequence = language
        .call(
            "lineRoute",
            "line",
            OwnLineCall::new("main", "timeline", "sequence", vec![]),
        )
        .await
        .unwrap();
    assert_eq!(
        sequence,
        serde_json::json!(session.sequence("main").unwrap())
    );
    let carried = serde_json::json!({
        "sessionId": session.id(),
        "lineId": "main",
        "service": "timeline",
        "method": "sequence",
        "args": [],
    });
    let error = language.refused("lineRoute", "line", carried).await;
    assert!(
        error.contains("unknown field") && error.contains("sessionId"),
        "a Session route names no Session: {error}"
    );
    assert!(
        language
            .call(
                "lineRoute",
                "line",
                OwnLineCall::new("main", "timeline", "sequence", vec![])
            )
            .await
            .is_ok(),
        "the Session's own line still answers {id}"
    );
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_unloaded_session_answers_no_session_until_it_loads_again() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();

    assert!(
        session.unload_if_idle().await.unwrap(),
        "an idle Session unloads"
    );
    assert_eq!(
        rig.route
            .session(SessionFaceCall::new(&id, "sessionConfig", "get", vec![]))
            .await
            .unwrap_err(),
        RouteError::NoSession(id.clone())
    );

    let restored = rig.host.restore_session(id.clone()).await.unwrap();
    assert!(
        rig.route
            .session(SessionFaceCall::new(&id, "sessionConfig", "get", vec![]))
            .await
            .is_ok()
    );
    assert!(
        rig.line_route(&restored)
            .line(OwnLineCall::new("main", "timeline", "sequence", vec![]))
            .await
            .is_ok()
    );
    restored.close().await.unwrap();
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_closed_by_business_answers_no_session_and_keeps_no_instance_to_hold() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();

    session.close().await.unwrap();
    assert_eq!(
        rig.route
            .session(SessionFaceCall::new(&id, "sessionConfig", "get", vec![]))
            .await
            .unwrap_err(),
        RouteError::NoSession(id.clone())
    );
    assert!(
        rig.host.get_session(&id).is_none(),
        "a closed Session has no live instance for a hold to keep"
    );
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closing_session_answers_session_closing_for_its_lines_too() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let line_route = rig.line_route(&session);
    let mut held = hold_close(&id, "main");
    let closing = {
        let session = session.clone();
        tokio::spawn(async move { session.close().await })
    };
    held.arrived().await;

    assert_eq!(
        rig.route
            .session(SessionFaceCall::new(&id, "sessionConfig", "get", vec![]))
            .await
            .unwrap_err(),
        RouteError::SessionClosing(id.clone())
    );
    assert_eq!(
        rig.route
            .line(LineFaceCall::new(
                &id,
                "main",
                "timeline",
                "sequence",
                vec![]
            ))
            .await
            .unwrap_err(),
        RouteError::SessionClosing(id.clone())
    );
    assert_eq!(
        line_route
            .line(OwnLineCall::new("main", "timeline", "sequence", vec![]))
            .await
            .unwrap_err(),
        RouteError::SessionClosing(id.clone())
    );
    held.release();
    closing.await.unwrap().unwrap();
    assert_eq!(
        rig.route
            .session(SessionFaceCall::new(&id, "sessionConfig", "get", vec![]))
            .await
            .unwrap_err(),
        RouteError::NoSession(id.clone())
    );
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_line_answers_no_line_and_the_sessions_other_calls_still_arrive() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let source = || BranchSource::Host {
        operation_id: "routing".into(),
    };
    let line = session
        .branch(
            "main".into(),
            0,
            BranchOptions {
                prompt: "a line the route reaches".into(),
                source: source(),
            },
        )
        .await
        .unwrap();
    assert!(
        rig.route
            .line(LineFaceCall::new(
                &id,
                &line,
                "timeline",
                "sequence",
                vec![]
            ))
            .await
            .is_ok()
    );

    session.end_branch(&line, source()).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let answer = rig
            .route
            .line(LineFaceCall::new(
                &id,
                &line,
                "timeline",
                "sequence",
                vec![],
            ))
            .await;
        if matches!(answer, Err(RouteError::NoLine { .. })) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the ended line closes: {answer:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        rig.route
            .session(SessionFaceCall::new(&id, "sessionConfig", "get", vec![]))
            .await
            .is_ok(),
        "one line's end does not affect the Session's other calls"
    );
    assert!(
        rig.route
            .line(LineFaceCall::new(
                &id,
                "main",
                "timeline",
                "sequence",
                vec![]
            ))
            .await
            .is_ok()
    );
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routing_resolves_only_data_faces_of_the_scope_it_reaches() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let route = rig.line_route(&session);

    for service in ["sessionMethods", "toolRegistry", "guideEntries"] {
        assert_eq!(
            rig.route
                .session(SessionFaceCall::new(&id, service, "register", vec![]))
                .await
                .unwrap_err(),
            RouteError::NotDataFace {
                service: service.into()
            },
            "{service} is a registration point"
        );
    }
    for service in ["sessionLifecycle", "branchLifecycle"] {
        assert_eq!(
            rig.route
                .session(SessionFaceCall::new(&id, service, "closing", vec![]))
                .await
                .unwrap_err(),
            RouteError::NotDataFace {
                service: service.into()
            },
            "{service} is a lifecycle service"
        );
    }
    assert_eq!(
        rig.route
            .line(LineFaceCall::new(&id, "main", "loopState", "state", vec![]))
            .await
            .unwrap_err(),
        RouteError::NotDataFace {
            service: "loopState".into()
        },
        "an internal face is not a data face"
    );
    assert_eq!(
        rig.route
            .session(SessionFaceCall::new(&id, "timeline", "sequence", vec![]))
            .await
            .unwrap_err(),
        RouteError::WrongScope {
            service: "timeline".into(),
            group: "session".into()
        },
        "a line face is not a Session face"
    );
    assert_eq!(
        route
            .line(OwnLineCall::new("main", "sessionConfig", "get", vec![]))
            .await
            .unwrap_err(),
        RouteError::WrongScope {
            service: "sessionConfig".into(),
            group: "branch".into()
        },
        "a Session face is not a line face"
    );
    let error = rig
        .route
        .line(LineFaceCall::new(&id, "main", "timeline", "absent", vec![]))
        .await
        .unwrap_err();
    let RouteError::Owner { name, message } = error else {
        panic!("the owner's failure: {error:?}");
    };
    assert_eq!(name, "no-method", "the owner's own failure keeps its name");
    assert!(message.contains("absent"), "{message}");
    rig.host.close().await.unwrap();
}
