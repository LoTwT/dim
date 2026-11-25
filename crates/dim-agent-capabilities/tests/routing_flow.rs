//! Flow: a call by id reaches the data face of a live Session or line, and
//! answers `NoSession`, `SessionClosing` or `NoLine` otherwise
//! ([#2127](https://github.com/arcships/dim/issues/2127)).

use dim_agent_capabilities::{defaults, methods::SessionMethods};
use dim_agent_runtime::testing::{MinimalHost, hold_close};
use dim_agent_runtime::*;
use dim_agent_types::BranchSource;
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::{
    Connection, Dispatch, Endpoint, Error as BridgeError, HostDispatch, Reply, Value, host_key,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(TimelineStoreBinding::new(Arc::new(
        MemoryTimelineStore::default(),
    )));
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder
}

async fn open(host: &MinimalHost) -> Session {
    host.create_session(SessionConfig::default())
        .await
        .expect("a Session opens")
}

/// A language runtime's side of a rutis session: it names a service, and the
/// host serves it.
struct Names(Ctx);

impl Dispatch for Names {
    fn invoke(&self, _: &Connection, target: &str, method: &str, args: Value) -> Reply {
        self.0
            .get_at::<dyn HostDispatch>(&host_key(target))
            .unwrap_or_else(|| panic!("{target} is not served"))
            .invoke(method, args)
    }
}

/// Both ends of a rutis session with the host's tree, kept open by the test.
struct Language {
    _host: Connection,
    client: Connection,
}

impl Language {
    fn open(root: &Ctx) -> Self {
        let (host, far) = rutis_bridge::memory::pair();
        let peer = |id: &str| Endpoint::rust(rutis_bridge::channel::PeerId::new(id).unwrap());
        let host =
            Connection::open(host, Arc::new(Names(root.clone())), peer("route-host")).unwrap();
        let client =
            Connection::open(far, Arc::new(Names(root.clone())), peer("route-client")).unwrap();
        Self {
            _host: host,
            client,
        }
    }

    async fn call(
        &self,
        method: &str,
        call: impl serde::Serialize,
    ) -> std::result::Result<serde_json::Value, BridgeError> {
        let call: Value = serde_json::to_value(call).unwrap().into();
        let reply = self
            .client
            .invoke_settled("route", method, Value::List(vec![call]))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), reply)
            .await
            .expect("the far end answers")?
            .json()
    }

    async fn refused(&self, method: &str, call: impl serde::Serialize) -> String {
        let error = self
            .call(method, call)
            .await
            .expect_err("the route refuses the call");
        error.to_string()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_by_id_reaches_the_live_session_and_line() {
    let builder = builder();
    let route = builder.route();
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = open(&host).await;
    let id = session.id().to_owned();

    let config = route.session_owner::<SessionConfig>(&id).unwrap();
    assert_eq!(
        serde_json::to_value(config.as_ref()).unwrap(),
        serde_json::to_value(session.config()).unwrap(),
        "the Session route reads the Session's own face"
    );
    let loop_state = route
        .line_owner::<dyn LoopState>(&id, "main")
        .expect("the main line's face");
    assert_eq!(loop_state.state().phase, LoopPhase::Idle);

    assert!(matches!(
        route.session_owner::<SessionConfig>("absent"),
        Err(AgentError::NoSession(absent)) if absent == "absent"
    ));
    assert!(matches!(
        route.line_owner::<dyn LoopState>(&id, "absent"),
        Err(AgentError::NoLine(line)) if line == format!("{id}/absent")
    ));

    let named = route
        .session(SessionFaceCall::new(&id, "sessionConfig", "get", vec![]))
        .await
        .unwrap();
    assert_eq!(
        named,
        serde_json::to_value(session.config()).unwrap(),
        "naming the face reaches what holding it does"
    );
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_language_call_reaches_a_line_face_through_the_route() {
    let builder = builder();
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = open(&host).await;
    let id = session.id().to_owned();
    let language = Language::open(host.root());
    language.client.ready().await.unwrap();

    let call = LineFaceCall::new(&id, "main", "timeline", "sequence", vec![]);
    let sequence = language.call("line", call).await.unwrap();
    assert_eq!(
        sequence,
        serde_json::json!(session.sequence("main").unwrap()),
        "the routed call reads the line's own face"
    );
    let missing = LineFaceCall::new(&id, "absent", "timeline", "sequence", vec![]);
    let error = language.refused("line", missing).await;
    assert!(error.contains("no line"), "{error}");
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_unloaded_session_answers_no_session_until_it_loads_again() {
    let builder = builder();
    let route = builder.route();
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = open(&host).await;
    let id = session.id().to_owned();

    assert!(
        session.unload_if_idle().await.unwrap(),
        "an idle Session unloads"
    );
    assert!(matches!(
        route.session_owner::<SessionConfig>(&id),
        Err(AgentError::NoSession(absent)) if absent == id
    ));

    let restored = host.restore_session(id.clone()).await.unwrap();
    assert!(route.session_owner::<SessionConfig>(&id).is_ok());
    assert!(route.line_owner::<dyn LoopState>(&id, "main").is_ok());
    restored.close().await.unwrap();
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_closed_by_business_answers_no_session_and_keeps_no_instance_to_hold() {
    let builder = builder();
    let route = builder.route();
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = open(&host).await;
    let id = session.id().to_owned();

    session.close().await.unwrap();
    assert!(matches!(
        route.session_owner::<SessionConfig>(&id),
        Err(AgentError::NoSession(absent)) if absent == id
    ));
    assert!(
        host.get_session(&id).is_none(),
        "a closed Session has no live instance for a hold to keep"
    );
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closing_session_answers_session_closing_for_its_lines_too() {
    let builder = builder();
    let route = builder.route();
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = open(&host).await;
    let id = session.id().to_owned();
    let mut held = hold_close(&id, "main");
    let closing = {
        let session = session.clone();
        tokio::spawn(async move { session.close().await })
    };
    held.arrived().await;

    assert!(matches!(
        route.session_owner::<SessionConfig>(&id),
        Err(AgentError::SessionClosing(closing)) if closing == id
    ));
    assert!(matches!(
        route.line_owner::<dyn LoopState>(&id, "main"),
        Err(AgentError::SessionClosing(closing)) if closing == id
    ));
    held.release();
    closing.await.unwrap().unwrap();
    assert!(matches!(
        route.session_owner::<SessionConfig>(&id),
        Err(AgentError::NoSession(absent)) if absent == id
    ));
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_line_answers_no_line_and_the_sessions_other_calls_still_arrive() {
    let builder = builder();
    let route = builder.route();
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = open(&host).await;
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
    assert!(route.line_owner::<dyn LoopState>(&id, &line).is_ok());

    session.end_branch(&line, source()).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !matches!(
        route.line_owner::<dyn LoopState>(&id, &line),
        Err(AgentError::NoLine(_))
    ) {
        assert!(Instant::now() < deadline, "the ended line closes");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        route.session_owner::<SessionConfig>(&id).is_ok(),
        "one line's end does not affect the Session's other calls"
    );
    assert!(route.line_owner::<dyn LoopState>(&id, "main").is_ok());
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routing_refuses_registration_points_and_lifecycle_services() {
    let builder = builder();
    let route = builder.route();
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = open(&host).await;
    let id = session.id().to_owned();

    let refused = route
        .session_owner::<SessionMethods>(&id)
        .err()
        .expect("a registration point is not routable");
    assert!(
        matches!(&refused, AgentError::InvalidInput(_)),
        "{refused:?}"
    );
    assert!(refused.to_string().contains("sessionMethods"), "{refused}");
    for service in [
        "sessionMethods",
        "toolRegistry",
        "sessionLifecycle",
        "branchLifecycle",
    ] {
        let error = route
            .session(SessionFaceCall::new(&id, service, "register", vec![]))
            .await
            .expect_err("the route refuses the name");
        assert!(
            matches!(&error, AgentError::InvalidInput(_)),
            "{service}: {error:?}"
        );
        assert!(error.to_string().contains(service), "{service}: {error}");
    }
    let process = route
        .session_owner::<dyn LoopState>(&id)
        .err()
        .expect("a line service is not a Session face");
    assert!(
        process.to_string().contains("not a session service"),
        "{process}"
    );
    host.close().await.unwrap();
}
