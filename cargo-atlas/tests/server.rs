//! End-to-end tests of `cargo atlas serve` over the real protocol. Each test
//! starts the binary as a child process and talks MCP to it on stdin and
//! stdout, the way Claude Code does.

mod common;

use std::path::Path;

use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

use common::{atlas, built, copy_of, fixture};

type Client = RunningService<RoleClient, ()>;

async fn connect(dir: &Path) -> Client {
    let command = tokio::process::Command::new(env!("CARGO_BIN_EXE_cargo-atlas")).configure(|c| {
        c.arg("serve").arg("--dir").arg(dir);
    });
    let transport = TokioChildProcess::new(command).expect("the server should start");
    ().serve(transport)
        .await
        .expect("the MCP handshake should succeed")
}

/// Calls a tool. Returns its text and whether it was marked as an error.
async fn call(client: &Client, tool: &'static str, arguments: Value) -> (String, bool) {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .expect("tools/call should get a result");
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.as_str())
        .collect::<String>();
    (text, result.is_error == Some(true))
}

async fn answer(client: &Client, tool: &'static str, arguments: Value) -> String {
    let (text, failed) = call(client, tool, arguments).await;
    assert!(!failed, "{tool} failed:\n{text}");
    text
}

#[tokio::test]
async fn the_server_names_itself_and_lists_its_tools() {
    if !built("spike") {
        return;
    }
    let client = connect(&fixture("spike")).await;
    let info = client
        .peer_info()
        .expect("the handshake returns server info");
    assert_eq!(
        info.server_info.as_ref().map(|i| i.name.as_str()),
        Some("cargo-atlas")
    );
    assert!(
        info.instructions
            .as_deref()
            .is_some_and(|i| i.contains("Prefer these tools to grep"))
    );
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .expect("tools/list")
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "callees",
            "callers",
            "explain",
            "impls",
            "path",
            "refresh",
            "search",
            "tests",
            "unsafe_code"
        ]
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn tool_answers_match_the_command_line_word_for_word() {
    if !built("spike") || !built("edge") {
        return;
    }
    let cases: [(&str, &'static str, Value, Vec<&str>); 10] = [
        (
            "spike",
            "callers",
            json!({"item": "JsonReader::parse"}),
            vec!["callers", "JsonReader::parse"],
        ),
        (
            "spike",
            "callees",
            json!({"item": "run"}),
            vec!["callees", "run"],
        ),
        (
            "spike",
            "impls",
            json!({"item": "Loader"}),
            vec!["impls", "Loader"],
        ),
        (
            "spike",
            "path",
            json!({"from": "main", "to": "CsvReader::parse"}),
            vec!["path", "main", "CsvReader::parse"],
        ),
        (
            "spike",
            "explain",
            json!({"item": "run"}),
            vec!["explain", "run"],
        ),
        (
            "spike",
            "search",
            json!({"text": "parse", "kind": "method"}),
            vec!["search", "parse", "--kind", "method"],
        ),
        (
            "edge",
            "tests",
            json!({"item": "first_unchecked"}),
            vec!["tests", "first_unchecked"],
        ),
        ("edge", "unsafe_code", json!({}), vec!["unsafe"]),
        (
            "edge",
            "unsafe_code",
            json!({"item": "first_or_zero"}),
            vec!["unsafe", "first_or_zero"],
        ),
        (
            "edge",
            "unsafe_code",
            json!({"missing_only": true}),
            vec!["unsafe", "--missing"],
        ),
    ];
    let spike = connect(&fixture("spike")).await;
    let edge = connect(&fixture("edge")).await;
    for (fixture_name, tool, arguments, cli_args) in cases {
        let client = if fixture_name == "spike" {
            &spike
        } else {
            &edge
        };
        let from_server = answer(client, tool, arguments).await;
        let from_cli = atlas(fixture_name, &cli_args);
        assert_eq!(from_server, from_cli, "{tool} {cli_args:?}");
    }
    spike.cancel().await.ok();
    edge.cancel().await.ok();
}

#[tokio::test]
async fn an_ambiguous_name_is_an_answer_but_an_unknown_name_is_an_error() {
    if !built("spike") {
        return;
    }
    let client = connect(&fixture("spike")).await;
    let (text, failed) = call(&client, "callers", json!({"item": "parse"})).await;
    assert!(!failed, "a list of choices is an answer:\n{text}");
    assert!(
        text.contains("matches 2 items")
            && text.contains("spike::csv_reader::CsvReader::parse  (src/csv_reader.rs:10)"),
        "{text}"
    );

    let (text, failed) = call(&client, "callers", json!({"item": "no_such_item"})).await;
    assert!(
        failed && text.contains("nothing named `no_such_item`"),
        "{text}"
    );

    let (text, failed) = call(&client, "callers", json!({"item": "CsvReader"})).await;
    assert!(failed && text.contains("is a struct"), "{text}");
    client.cancel().await.ok();
}

#[tokio::test]
async fn an_edit_is_noticed_and_refresh_brings_the_graph_up_to_date() {
    if !built("spike") {
        return;
    }
    let dir = copy_of("spike", "edited-spike");
    let client = connect(&dir).await;

    // No graph yet: the first call builds one and waits for it.
    let before = answer(&client, "callers", json!({"item": "JsonReader::parse"})).await;
    assert!(dir.join(".atlas/graph.json").is_file());
    assert!(!before.contains("Note:"), "{before}");
    assert!(!before.contains("parse_twice"), "{before}");

    // Add a caller of JsonReader::parse, as an assistant editing code would.
    let file = dir.join("src/json_reader.rs");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\npub fn parse_twice(r: &JsonReader) -> usize {\n    r.parse().len() * 2\n}\n");
    std::fs::write(&file, text).unwrap();

    // The answer still comes from the old graph, and says which file changed.
    let stale = answer(&client, "callers", json!({"item": "JsonReader::parse"})).await;
    assert!(
        stale.starts_with("Note: 1 file changed (src/json_reader.rs) since the graph was built"),
        "{stale}"
    );
    assert!(stale.contains("call `refresh`"), "{stale}");

    // `refresh` waits for the rebuild; afterwards the new caller is there.
    let refreshed = answer(&client, "refresh", json!({})).await;
    assert!(
        refreshed.starts_with("Wrote .atlas/graph.json") || refreshed.starts_with("A rebuild"),
        "{refreshed}"
    );
    let after = answer(&client, "callers", json!({"item": "JsonReader::parse"})).await;
    assert!(!after.contains("Note:"), "{after}");
    assert!(after.contains("<- parse_twice "), "{after}");
    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_folder_that_is_not_a_cargo_workspace_gets_an_error_not_a_crash() {
    // Not under target/: Cargo would find this repo's own Cargo.toml above it.
    let dir = std::env::temp_dir().join(format!("cargo-atlas-plain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let client = connect(&dir).await;
    let (text, failed) = call(&client, "callers", json!({"item": "main"})).await;
    assert!(
        failed && text.contains("is not a Cargo workspace"),
        "{text}"
    );
    // Still serving: the tool list works after the error.
    assert!(!client.list_all_tools().await.unwrap().is_empty());
    client.cancel().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}
