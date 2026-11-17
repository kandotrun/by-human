//! Development-only MCP peer. It exposes no host operation and uses only
//! synthetic input; normal by-human builds do not include this binary.
use std::{
    fs::OpenOptions,
    io::{self, BufRead, Write},
    path::PathBuf,
};

use serde_json::{Value, json};

fn journal(path: &Option<PathBuf>, method: &str, tool: Option<&str>) {
    if let Some(path) = path {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("fixture journal must be writable");
        let entry = json!({
            "method": method,
            "tool": tool,
            "pid": std::process::id(),
            "inherited_credentials": inherited_credentials(),
        });
        writeln!(file, "{entry}").unwrap();
    }
}

fn inherited_credentials() -> Vec<&'static str> {
    ["OPENAI_API_KEY", "BY_HUMAN_API_TOKEN", "DATABASE_URL"]
        .into_iter()
        .filter(|name| std::env::var_os(name).is_some())
        .collect()
}

fn definitions() -> Value {
    let mut tools = vec![json!({
        "name": "echo_note",
        "description": "Return the supplied synthetic note unchanged.",
        "inputSchema": {
            "type": "object", "properties": {"text": {"type":"string"}},
            "required": ["text"], "additionalProperties": false,
        },
        "annotations": {"readOnlyHint": true},
    })];
    for name in ["fail_note", "hang_note", "tool_not_allowed"] {
        tools.push(json!({
            "name": name,
            "description": "Synthetic contract fixture with no host operation.",
            "inputSchema": {"type":"object", "properties":{}, "additionalProperties":false},
            "annotations": {"readOnlyHint": true},
        }));
    }
    json!({"tools":tools})
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mut path = None;
    let mut hang_initialize = false;
    let mut args = args.into_iter();
    while let Some(option) = args.next() {
        if option == "--journal" {
            path = Some(PathBuf::from(args.next().expect("fixture journal path")));
        } else if option == "--hang-initialize" {
            hang_initialize = true;
        } else {
            std::process::exit(2);
        }
    }
    journal(&path, "fixture.started", None);
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(request): Result<Value, _> = serde_json::from_str(&line) else {
            continue;
        };
        let Some(method) = request["method"].as_str() else {
            continue;
        };
        let tool = request["params"]["name"].as_str();
        journal(&path, method, tool);
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match method {
            "initialize" if hang_initialize => continue,
            "initialize" => json!({
                "protocolVersion": request["params"]["protocolVersion"],
                "capabilities": {"tools":{}},
                "serverInfo": {"name":"by-human-mcp-contract", "version":"0.0.1"},
            }),
            "ping" => json!({}),
            "tools/list" => definitions(),
            "tools/call" => match tool {
                Some("echo_note") => {
                    let output = json!({
                        "echo": request["params"]["arguments"]["text"],
                        "inherited_credentials": inherited_credentials(),
                    });
                    json!({
                        "content":[{"type":"text", "text":output.to_string()}],
                        "structuredContent":output,
                        "isError":false,
                    })
                }
                Some("fail_note") => json!({
                    "content":[{"type":"text", "text":"Synthetic operation was refused."}],
                    "structuredContent":{"code":"FIXTURE_REFUSED"},
                    "isError":true,
                }),
                // Acceptance is recorded before intentionally withholding the
                // result. Continue reading notifications so cancellation and
                // process shutdown exercise a real live MCP connection.
                Some("hang_note") => continue,
                _ => json!({
                    "content":[{"type":"text", "text":"Unexpected fixture tool invocation."}],
                    "isError":true,
                }),
            },
            _ => {
                let response = json!({"jsonrpc":"2.0", "id":id,
                    "error":{"code":-32601, "message":"Unknown fixture method"}});
                writeln!(stdout, "{response}").unwrap();
                stdout.flush().unwrap();
                continue;
            }
        };
        let response = json!({"jsonrpc":"2.0", "id":id, "result":result});
        writeln!(stdout, "{response}").unwrap();
        stdout.flush().unwrap();
    }
}
