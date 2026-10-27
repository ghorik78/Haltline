//! Tests of the `haltline-dap` binary that need no external tools.

mod common;

use common::DapClient;
use serde_json::json;
use std::process::Command;

#[test]
fn prints_version_and_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_haltline-dap"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("haltline-dap {}", env!("CARGO_PKG_VERSION"))
    );
    let out = Command::new(env!("CARGO_BIN_EXE_haltline-dap"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("Debug Adapter Protocol"));
    let out = Command::new(env!("CARGO_BIN_EXE_haltline-dap"))
        .arg("--bogus")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn speaks_dap_over_stdio() {
    let mut c = DapClient::spawn();
    let caps = c.request(
        "initialize",
        json!({"adapterID": "haltline", "clientID": "zed"}),
    );
    assert_eq!(caps["supportsConfigurationDoneRequest"], true);
    assert_eq!(caps["supportsDisassembleRequest"], true);

    // Requests before launch fail cleanly instead of crashing the adapter.
    let resp = c.request_raw("stackTrace", json!({"threadId": 1}));
    assert_eq!(resp["success"], false);
    let resp = c.request_raw("launch", json!({}));
    assert_eq!(resp["success"], false);
    assert!(resp["message"].as_str().unwrap().contains("executable"));
    let resp = c.request_raw(
        "launch",
        json!({"executable": "/fw.elf", "serverType": "nonsense"}),
    );
    assert_eq!(resp["success"], false);
    assert!(
        resp["message"].as_str().unwrap().contains("serverType")
            || resp["message"]
                .as_str()
                .unwrap()
                .contains("unknown variant")
    );
    let threads = c.request("threads", json!({}));
    assert_eq!(threads["threads"][0]["id"], 1);
    let resp = c.request_raw("someFutureRequest", json!({}));
    assert_eq!(resp["success"], false);
    c.disconnect();
}

#[test]
fn exits_when_the_client_goes_away() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_haltline-dap"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "adapter kept running without a client"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
