//! Session tool mode (agent tool modes §5): `session.create.toolMode`,
//! `session.tool_mode.get` and `.update` over public RPC. The assembly mounts
//! neither entry tool, so a chosen `codemode` or `shell` reports `normal` as
//! the mode in force.
use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::{AgentBuilder, AgentConfig, Session};
use dim_agent_types::{EventKind, ModelRequest};
use dim_app_server::{RunningServer, ServerConfig, serve};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = PathBuf::from("/tmp").join(format!("dim-mode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("work")).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(directory.canonicalize().unwrap())
    }
    fn work(&self) -> PathBuf {
        self.0.join("work")
    }
    fn database(&self) -> PathBuf {
        self.0.join("dimcode.sqlite")
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn builder(codemode: bool) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    if codemode {
        dim_codemode::install(&mut builder);
    }
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder
}

async fn launch(home: &Home, codemode: bool) -> RunningServer {
    let models = AimuxModels::from_config(
        serde_json::from_value::<ModelConfigFile>(json!({
            "defaultModel": {"providerId": "local", "modelId": "scripted"},
            "providers": [{"id": "local", "kind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "models": [{"id": "scripted", "contextWindow": 128000, "maxOutputTokens": 4096, "config": {"maxTokens": 64}}]}]
        }))
        .unwrap(),
    )
    .unwrap();
    let config = ServerConfig {
        unix_path: Some(home.0.join("server.sock")),
        websocket_address: None,
        target_home: Some(home.0.clone()),
        model_catalog: Some(Arc::new(models)),
        ..Default::default()
    };
    serve(builder(codemode), config).await.unwrap()
}

struct Client {
    stream: BufReader<UnixStream>,
    id: u64,
}
impl Client {
    async fn connect(server: &RunningServer) -> Self {
        let stream = UnixStream::connect(server.unix_path.as_ref().unwrap())
            .await
            .unwrap();
        let mut client = Self {
            stream: BufReader::new(stream),
            id: 0,
        };
        client
            .rpc(
                "hello",
                json!({"protocolVersion": 4, "clientKind": "other"}),
            )
            .await;
        client
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let request = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        self.stream
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let stream = &mut self.stream;
        tokio::time::timeout(Duration::from_secs(30), async move {
            loop {
                let mut line = String::new();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                let value: Value = serde_json::from_str(&line).unwrap();
                if value["id"].as_u64() == Some(request["id"].as_u64().unwrap()) {
                    return value;
                }
            }
        })
        .await
        .expect("response deadline")
    }
    async fn create(&mut self, home: &Home, extra: Value) -> String {
        let mut params = json!({"cwd": home.work(), "tools": []});
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        let created = self.rpc("session.create", params).await;
        created["result"]["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("{created}"))
            .to_owned()
    }
    async fn get(&mut self, session: &str) -> Value {
        self.rpc("session.tool_mode.get", json!({"sessionId": session}))
            .await
    }
    async fn update(&mut self, session: &str, mode: &str) -> Value {
        self.rpc(
            "session.tool_mode.update",
            json!({"sessionId": session, "mode": mode}),
        )
        .await
    }
}

fn live_session(server: &RunningServer, id: &str) -> Session {
    server.agent().unwrap().get_session(id).unwrap()
}

async fn last_request(session: &Session) -> Arc<ModelRequest> {
    chain_events(session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .next_back()
        .expect("a model request")
}

fn stored_config(home: &Home, session: &str) -> String {
    let connection = rusqlite::Connection::open(home.database()).unwrap();
    connection
        .query_row(
            "SELECT config FROM agent_sessions WHERE session_id = ?1",
            [session],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_session_starts_normal_and_falls_back_when_the_entry_tool_is_absent() {
    let home = Home::new();
    let server = launch(&home, false).await;
    let mut client = Client::connect(&server).await;
    let session = client.create(&home, json!({})).await;

    let read = client.get(&session).await;
    assert_eq!(read["result"]["mode"], "normal", "{read}");
    assert_eq!(read["result"]["effective"], "normal", "{read}");

    let updated = client.update(&session, "shell").await;
    assert_eq!(updated["result"]["mode"], "shell", "{updated}");
    assert_eq!(
        updated["result"]["effective"], "normal",
        "the assembly mounts no execute, so shell is not in force: {updated}"
    );
    let read = client.get(&session).await;
    assert_eq!(read["result"]["mode"], "shell", "{read}");
    assert_eq!(read["result"]["effective"], "normal", "{read}");
    assert!(
        stored_config(&home, &session).contains("\"toolMode\":\"shell\""),
        "the record carries the choice"
    );

    let updated = client.update(&session, "normal").await;
    assert_eq!(updated["result"]["mode"], "normal", "{updated}");
    assert_eq!(updated["result"]["effective"], "normal", "{updated}");
    assert!(
        !stored_config(&home, &session).contains("toolMode"),
        "normal is omitted from the record"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_records_the_operators_choice() {
    let home = Home::new();
    let server = launch(&home, false).await;
    let mut client = Client::connect(&server).await;
    let session = client.create(&home, json!({"toolMode": "codemode"})).await;

    let read = client.get(&session).await;
    assert_eq!(read["result"]["mode"], "codemode", "{read}");
    assert_eq!(
        read["result"]["effective"], "normal",
        "the assembly mounts no codemode, so the chosen mode is not in force: {read}"
    );
    assert!(
        stored_config(&home, &session).contains("\"toolMode\":\"codemode\""),
        "the record carries the creation choice"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chosen_mode_without_its_entry_tool_declares_the_normal_surface() {
    let home = Home::new();
    let server = launch(&home, false).await;
    let mut client = Client::connect(&server).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": home.work(), "toolMode": "codemode"}),
        )
        .await;
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "hello"}),
        )
        .await;
    assert!(submitted.get("error").is_none(), "{submitted}");
    let idle = client
        .rpc(
            "session.idle",
            json!({"sessionId": session, "branchId": "main"}),
        )
        .await;
    assert!(idle.get("error").is_none(), "{idle}");
    let request = last_request(&live_session(&server, &session)).await;
    let names = request
        .tools
        .as_ref()
        .expect("a normal request declares tools")
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    assert!(
        !names.is_empty(),
        "the normal surface is not empty: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "codemode"),
        "normal hides the entry tool: {names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codemode_declares_only_its_entry_and_skill() {
    let home = Home::new();
    let server = launch(&home, true).await;
    let mut client = Client::connect(&server).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": home.work(), "toolMode": "codemode", "tools": ["codemode", "skill"]}),
        )
        .await;
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    let mode = client.get(&session).await;
    assert_eq!(mode["result"]["effective"], "codemode", "{mode}");
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "hello"}),
        )
        .await;
    assert!(submitted.get("error").is_none(), "{submitted}");
    let idle = client
        .rpc(
            "session.idle",
            json!({"sessionId": session, "branchId": "main"}),
        )
        .await;
    assert!(idle.get("error").is_none(), "{idle}");
    let request = last_request(&live_session(&server, &session)).await;
    let mut names = request
        .tools
        .as_ref()
        .expect("a codemode request declares tools")
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["codemode"], "{names:?}");
}
