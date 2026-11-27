#![cfg(windows)]

use dim_client::pipe;
use std::{io, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use uuid::Uuid;

#[tokio::test]
async fn closing_one_rpc_pipe_releases_its_target_while_another_keeps_working() {
    let temp = tempfile::tempdir().unwrap();
    let ready = temp.path().join("ready");
    let target = format!(r"\\.\pipe\dim-test-target-{}", Uuid::new_v4());
    let proxy = format!(r"\\.\pipe\dim-test-proxy-{}", Uuid::new_v4());
    let mut first_target = pipe::bind(&target, true).unwrap();
    let (target_in, proxy_out) = io::pipe().unwrap();
    let (proxy_in, target_out) = io::pipe().unwrap();
    let binary = env!("CARGO_BIN_EXE_dim-terminal-transport");
    let mut target_host = Command::new(binary)
        .args(["serve", &target])
        .stdin(target_in)
        .stdout(target_out)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut proxy_host = Command::new(binary)
        .args(["connect", &proxy])
        .arg(&ready)
        .stdin(proxy_in)
        .stdout(proxy_out)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready.is_file() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let first = pipe::connect(&proxy).await.unwrap();
        first_target.connect().await.unwrap();
        let mut second_target = pipe::bind(&target, false).unwrap();
        let mut second = pipe::connect(&proxy).await.unwrap();
        second_target.connect().await.unwrap();

        drop(first);
        assert_eq!(first_target.read(&mut [0; 1]).await.unwrap(), 0);
        second.write_all(b"request").await.unwrap();
        let mut request = [0; 7];
        second_target.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"request");
        second_target.write_all(b"reply").await.unwrap();
        let mut reply = [0; 5];
        second.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"reply");
    })
    .await
    .expect("a completed RPC releases only its own target pipe");
    proxy_host.kill().await.unwrap();
    let _ = target_host.kill().await;
}
