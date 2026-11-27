use crate::{
    Result,
    args::Options,
    client,
    model::session::{SessionPort, Update},
    session_host::SessionHost,
    sharing,
};
use dim_client::Client;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    runtime::Handle,
};

#[derive(Deserialize)]
pub(crate) struct Device {
    pub name: String,
    pub addresses: Vec<String>,
    pub verified: bool,
}

#[derive(Clone)]
pub(crate) struct Devices {
    pub options: Options,
    pub runtime: Handle,
    local_workspace: String,
}

#[derive(Debug)]
pub(crate) struct Pairing(String);
impl std::fmt::Display for Pairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Pairing {}
#[derive(Deserialize)]
#[serde(untagged)]
enum TunnelReply {
    Ready(Remote),
    Pair { pairing: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Remote {
    name: String,
    address: String,
    identity: String,
    endpoint: PathBuf,
    working_directory: String,
}

pub(crate) struct Target {
    pub name: String,
    pub id: String,
    pub address: Option<String>,
    options: Options,
    config: dim_client::ConnectionConfig,
    _lifetime: Option<ChildStdin>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    pub session_id: String,
    pub title: Option<String>,
    pub cwd: String,
    pub running: bool,
}

#[derive(Deserialize)]
pub(crate) struct Directories {
    pub path: Option<String>,
    pub parent: Option<String>,
    pub directories: Vec<Directory>,
}

#[derive(Deserialize)]
pub(crate) struct Directory {
    pub name: String,
    pub path: String,
}

impl Devices {
    pub fn new(options: Options) -> Self {
        Self {
            local_workspace: options.local_workspace(),
            options,
            runtime: Handle::current(),
        }
    }

    pub async fn list(&self, discover: bool) -> Result<Vec<Device>> {
        let output = Command::from(sharing::command(&self.options.home)?)
            .arg(if discover { "discover" } else { "known" })
            .kill_on_drop(true)
            .output()
            .await?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    pub async fn connect(
        &self,
        address: Option<String>,
        token: String,
        remember: bool,
    ) -> Result<Arc<Target>> {
        let mut options = self.options.clone();
        let mut lifetime = None;
        let (name, id, route, workspace, endpoint) = if let Some(address) = address {
            #[derive(Serialize)]
            struct Pair {
                token: String,
                remember: bool,
            }
            let mut child = Command::from(sharing::command(&options.home)?)
                .args(["tunnel", &address])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let mut input = child.stdin.take().expect("piped input");
            let mut bytes = serde_json::to_vec(&Pair { token, remember })?;
            bytes.push(b'\n');
            input.write_all(&bytes).await?;
            let out = child.stdout.take().expect("piped output");
            let mut stderr = child.stderr.take().expect("piped errors");
            let ended = tokio::spawn(async move {
                let mut message = String::new();
                let _ = stderr.read_to_string(&mut message).await;
                let _ = child.wait().await;
                message
            });
            let mut line = String::new();
            if tokio::time::timeout(
                Duration::from_secs(30),
                BufReader::new(out).read_line(&mut line),
            )
            .await??
                == 0
            {
                drop(input);
                return Err(ended.await?.trim().to_owned().into());
            }
            let remote = match serde_json::from_str(&line)? {
                TunnelReply::Ready(remote) => remote,
                TunnelReply::Pair { pairing } => return Err(Box::new(Pairing(pairing))),
            };
            lifetime = Some(input);
            (
                remote.name,
                remote.identity,
                Some(remote.address),
                remote.working_directory,
                Some(remote.endpoint),
            )
        } else {
            (
                "This computer".into(),
                "local".into(),
                None,
                self.local_workspace.clone(),
                None,
            )
        };
        options.select_workspace(workspace, endpoint);
        let locale = crate::ui::i18n::interface_locale(&options.home);
        let link = client::connect_inner(&options, true, locale)
            .await?
            .ok_or("Computer is unavailable")?;
        Ok(Arc::new(Target {
            name,
            id,
            address: route,
            options,
            config: link.client.connection_config().clone(),
            _lifetime: lifetime,
        }))
    }

    pub fn initial(&self, client: &Client) -> Arc<Target> {
        let remote = self.options.endpoint.is_some();
        let read = |key| std::env::var(key).ok();
        Arc::new(Target {
            name: if remote {
                read("DIMCODE_TERMINAL_NAME").unwrap_or_else(|| "Remote computer".into())
            } else {
                "This computer".into()
            },
            id: if remote {
                read("DIMCODE_TERMINAL_ID").unwrap_or_else(|| client.instance_id().into())
            } else {
                "local".into()
            },
            address: remote.then(|| read("DIMCODE_TERMINAL_ADDRESS")).flatten(),
            options: self.options.clone(),
            config: client.connection_config().clone(),
            _lifetime: None,
        })
    }
}

impl Target {
    pub(crate) fn keyboard_dir(&self) -> &std::path::Path {
        self.options.launch_dir()
    }

    pub async fn sessions(&self) -> Result<Vec<Session>> {
        Client::connect(&self.config)
            .await?
            .typed(
                "session.active",
                serde_json::Value::Object(Default::default()),
            )
            .await
    }

    pub async fn directories(&self, path: Option<String>) -> Result<Directories> {
        Directories::read(&mut Client::connect(&self.config).await?, path).await
    }

    pub async fn open(self: Arc<Self>, cwd: String, session: Option<String>) -> Result<Prepared> {
        let mut options = self.options.clone();
        options.select_workspace(cwd.clone(), options.endpoint.clone());
        options.resume = session.clone();
        let client = Client::connect(&self.config).await?;
        let locale = crate::ui::i18n::interface_locale(&options.home);
        let host = SessionHost::start(options, client, locale.code(), None);
        let mut updates = Vec::new();
        if let Some(id) = &session {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                if std::time::Instant::now() >= deadline {
                    return Err("Timed out opening session history".into());
                }
                let batch = host.drain();
                let mut ready = false;
                for u in &batch {
                    match u {
                        Update::Failed(e) | Update::StartupFailed { message: e, .. } => {
                            return Err(e.clone().into());
                        }
                        Update::Shutdown { reason } => return Err(reason.clone().into()),
                        Update::Snapshot(s) if s.session_id == *id => {
                            if let Some(e) = &s.window_error {
                                return Err(e.clone().into());
                            }
                            ready |= s.history_complete;
                        }
                        _ => {}
                    }
                }
                updates.extend(batch);
                if ready {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        Ok(Prepared {
            target: self,
            cwd,
            session,
            host,
            updates,
        })
    }
}

pub(crate) struct Prepared {
    pub target: Arc<Target>,
    pub cwd: String,
    pub session: Option<String>,
    pub host: SessionHost,
    pub updates: Vec<Update>,
}

impl Directories {
    pub async fn read(client: &mut Client, path: Option<String>) -> Result<Self> {
        #[derive(Serialize)]
        struct Request {
            path: Option<String>,
        }
        client
            .typed(
                "workspace.files.directories",
                serde_json::to_value(Request { path })?,
            )
            .await
    }
}
