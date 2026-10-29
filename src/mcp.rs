//! `imkr mcp`: imkr as an MCP server on stdin/stdout, so agents get its
//! commands as tools. Each tool runs this binary's own command line, with
//! text on stdin, never through a shell.

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// The protocol version answered when the client asks for none.
const PROTOCOL: &str = "2025-06-18";

/// The tools: name, description, input schema.
fn tools() -> Value {
    let pane =
        json!({ "type": "integer", "description": "The pane number, as imkr_panes lists it." });
    let host = json!({ "type": "string", "description": "Another of the user's machines, as imkr_agents names it; this one if left out." });
    let monitor = json!({ "type": "string", "description": "Which screen, as imkr_screen_status lists them; `all` for every screen at once." });
    json!([
        {
            "name": "imkr_agents",
            "description": "Every coding agent in the imkr sessions on this machine (with all: also on the user's other machines): its address for imkr_message, state (working, blocked, idle, done), window and program.",
            "inputSchema": { "type": "object", "properties": {
                "all": { "type": "boolean", "description": "Also the agents on the user's other machines." }
            }}
        },
        {
            "name": "imkr_panes",
            "description": "The panes of this imkr session: number, window, program, agent state and directory.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "imkr_read",
            "description": "The text on a pane's screen, or its last lines including the history.",
            "inputSchema": { "type": "object", "required": ["pane"], "properties": {
                "pane": pane,
                "lines": { "type": "integer", "description": "How many of the last lines, history included." }
            }}
        },
        {
            "name": "imkr_send",
            "description": "Types text into a pane, followed by Enter unless enter is false. For shells and programs; to ask another agent something, use imkr_message.",
            "inputSchema": { "type": "object", "required": ["pane", "text"], "properties": {
                "pane": pane,
                "text": { "type": "string" },
                "enter": { "type": "boolean", "description": "Press Enter after the text (default true)." }
            }}
        },
        {
            "name": "imkr_message",
            "description": "Sends a message to another agent, typed in once it is idle, never into a question it asks the user. The address is a pane number, name or program in this session, session/pane, or machine:session/pane (see imkr_agents). With wait, waits for its answer and returns its screen.",
            "inputSchema": { "type": "object", "required": ["to", "text"], "properties": {
                "to": { "type": "string" },
                "text": { "type": "string" },
                "wait": { "type": "boolean" }
            }}
        },
        {
            "name": "imkr_run",
            "description": "Runs a command in a pane's shell and returns its output and exit code, like running it in your own shell. The shell needs imkr's shell integration (OSC 133); the error says how to set it up.",
            "inputSchema": { "type": "object", "required": ["pane", "command"], "properties": {
                "pane": pane,
                "command": { "type": "string" },
                "timeout": { "type": "integer", "description": "Seconds to wait at most." }
            }}
        },
        {
            "name": "imkr_wait",
            "description": "Waits for a pane: with until (working, blocked, idle, done or ready) for its agent to reach that state, without until for the command running in its shell to end (then returns its output).",
            "inputSchema": { "type": "object", "required": ["pane"], "properties": {
                "pane": pane,
                "until": { "type": "string", "enum": ["working", "blocked", "idle", "done", "failed", "ready"] },
                "timeout": { "type": "integer", "description": "Seconds to wait at most." }
            }}
        },
        {
            "name": "imkr_screen_status",
            "description": "Whether you may see and drive a machine's screen: the screens with their logical size and position, the tools, whether the session is locked, and the permission in force. Ask this before the other screen tools — it says what to do when the answer is no.",
            "inputSchema": { "type": "object", "properties": {
                "host": host.clone()
            }}
        },
        {
            "name": "imkr_screen_shot",
            "description": "A screenshot of the user's screen as a picture, scaled down. Its text says how an image pixel maps onto the coordinates imkr_screen_click takes, and which screens there are. Without monitor the screen set for agents, else the focused one; `all` for every screen at once.",
            "inputSchema": { "type": "object", "properties": {
                "monitor": monitor.clone(),
                "region": { "type": "string", "description": "A piece of the desktop as X,Y,WxH in logical coordinates, e.g. 3072,0,1280x800 — for reading small text, which the scaled-down whole screen loses." },
                "width": { "type": "integer", "description": "How wide the picture should be, in pixels (1280 by default)." },
                "host": host.clone()
            }}
        },
        {
            "name": "imkr_screen_click",
            "description": "Clicks on the screen. The coordinates are logical, as imkr_screen_shot reports them; with monitor they count from that screen's top left corner. Without coordinates it clicks where the pointer is.",
            "inputSchema": { "type": "object", "properties": {
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "button": { "type": "string", "enum": ["left", "right", "middle"] },
                "monitor": monitor.clone(),
                "host": host.clone()
            }}
        },
        {
            "name": "imkr_screen_move",
            "description": "Moves the pointer there, without clicking, and says where it ended up. Logical coordinates like imkr_screen_click.",
            "inputSchema": { "type": "object", "required": ["x", "y"], "properties": {
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "monitor": monitor.clone(),
                "host": host.clone()
            }}
        },
        {
            "name": "imkr_screen_type",
            "description": "Types text into whatever has the keyboard focus on that screen. Click the field first. Never type a password or anything secret: it goes to the window that happens to be focused, and imkr cannot tell what that is.",
            "inputSchema": { "type": "object", "required": ["text"], "properties": {
                "text": { "type": "string" },
                "host": host.clone()
            }}
        },
        {
            "name": "imkr_screen_key",
            "description": "Presses keys on that screen, e.g. `ctrl+c`, `Return`, `alt+Tab`, `super+1`. Several are pressed one after the other.",
            "inputSchema": { "type": "object", "required": ["keys"], "properties": {
                "keys": { "type": "array", "items": { "type": "string" }, "description": "The keys, e.g. [\"ctrl+l\", \"Return\"]." },
                "host": host.clone()
            }}
        },
        {
            "name": "imkr_new_pane",
            "description": "Starts a command (a shell if none) in a new pane of this session and returns its number.",
            "inputSchema": { "type": "object", "properties": {
                "command": { "type": "array", "items": { "type": "string" }, "description": "The program and its arguments." },
                "window": { "type": "integer", "description": "The window, 1 to 9; the current one if not given." }
            }}
        }
    ])
}

/// The command line and stdin that `tool` with `args` runs, or why not.
fn command_for(tool: &str, args: &Value) -> Result<(Vec<String>, String), String> {
    let pane = || {
        args["pane"]
            .as_u64()
            .map(|p| p.to_string())
            .ok_or("pane is needed".to_owned())
    };
    let text = |key: &str| {
        args[key]
            .as_str()
            .map(str::to_owned)
            .ok_or(format!("{key} is needed"))
    };
    let mut line: Vec<String> = Vec::new();
    let mut input = String::new();
    let push =
        |line: &mut Vec<String>, parts: &[&str]| line.extend(parts.iter().map(|s| (*s).to_owned()));
    match tool {
        "imkr_agents" => {
            push(&mut line, &["agents", "--json"]);
            if args["all"].as_bool() == Some(true) {
                push(&mut line, &["--all"]);
            }
        }
        "imkr_panes" => push(&mut line, &["list", "--json"]),
        "imkr_read" => {
            push(&mut line, &["read", "--pane", &pane()?]);
            if let Some(lines) = args["lines"].as_u64() {
                push(&mut line, &["--lines", &lines.to_string()]);
            }
        }
        "imkr_send" => {
            push(&mut line, &["send", "--stdin", "--pane", &pane()?]);
            if args["enter"].as_bool() == Some(false) {
                push(&mut line, &["--no-enter"]);
            }
            input = text("text")?;
        }
        "imkr_message" => {
            push(&mut line, &["msg", "--stdin"]);
            if args["wait"].as_bool() == Some(true) {
                push(&mut line, &["--wait"]);
            }
            line.push(text("to")?);
            input = text("text")?;
        }
        "imkr_run" => {
            push(&mut line, &["run", "--pane", &pane()?]);
            if let Some(t) = args["timeout"].as_u64() {
                push(&mut line, &["--timeout", &t.to_string()]);
            }
            line.push("--".into());
            line.push(text("command")?);
        }
        "imkr_wait" => {
            push(&mut line, &["wait", "--pane", &pane()?]);
            if let Some(until) = args["until"].as_str() {
                push(&mut line, &["--until", until]);
            }
            if let Some(t) = args["timeout"].as_u64() {
                push(&mut line, &["--timeout", &t.to_string()]);
            }
        }
        // `--host` goes in front: `imkr --host H screen …`.
        "imkr_screen_status" | "imkr_screen_shot" | "imkr_screen_click" | "imkr_screen_move"
        | "imkr_screen_type" | "imkr_screen_key" => {
            if let Some(host) = args["host"].as_str().filter(|h| !h.is_empty()) {
                push(&mut line, &["--host", host]);
            }
            let what = tool.trim_start_matches("imkr_screen_");
            push(&mut line, &["screen", what]);
            match what {
                "status" => push(&mut line, &["--json"]),
                "shot" => {
                    push(&mut line, &["--json"]);
                    if let Some(width) = args["width"].as_u64() {
                        push(&mut line, &["--width", &width.to_string()]);
                    }
                    if let Some(region) = args["region"].as_str().filter(|r| !r.is_empty()) {
                        push(&mut line, &["--region", region]);
                    }
                }
                "move" | "click" => {
                    // A click without coordinates lands where the pointer
                    // already is; one with them moves there first.
                    match (args["x"].as_i64(), args["y"].as_i64()) {
                        (Some(x), Some(y)) => {
                            push(&mut line, &[&x.to_string(), &y.to_string()]);
                        }
                        (None, None) if what == "click" => {}
                        _ => return Err("both x and y are needed".to_owned()),
                    }
                    if let Some(button) = args["button"].as_str() {
                        push(&mut line, &["--button", button]);
                    }
                }
                "type" => {
                    push(&mut line, &["--stdin"]);
                    input = text("text")?;
                }
                "key" => {
                    let keys: Vec<&str> = args["keys"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|k| k.as_str())
                        .collect();
                    if keys.is_empty() {
                        return Err("which keys? e.g. [\"ctrl+c\"]".to_owned());
                    }
                    push(&mut line, &keys);
                }
                _ => {}
            }
            if matches!(what, "shot" | "click" | "move")
                && let Some(name) = args["monitor"].as_str().filter(|n| !n.is_empty())
            {
                push(&mut line, &["--monitor", name]);
            }
        }
        "imkr_new_pane" => {
            push(&mut line, &["new"]);
            if let Some(w) = args["window"].as_u64() {
                push(&mut line, &["--window", &w.to_string()]);
            }
            if let Some(command) = args["command"].as_array().filter(|c| !c.is_empty()) {
                line.push("--".into());
                line.extend(command.iter().filter_map(|c| c.as_str()).map(str::to_owned));
            }
        }
        other => return Err(format!("no tool {other}")),
    }
    Ok((line, input))
}

/// Runs `tool`: its output, and whether it failed.
fn call(tool: &str, args: &Value) -> (String, bool) {
    let (line, input) = match command_for(tool, args) {
        Ok(found) => found,
        Err(e) => return (e, true),
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return (format!("cannot find imkr: {e}"), true),
    };
    let child = Command::new(exe)
        .args(&line)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return (format!("cannot run imkr: {e}"), true),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let Ok(output) = child.wait_with_output() else {
        return ("imkr did not finish".into(), true);
    };
    let mut text = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_owned();
    let errors = String::from_utf8_lossy(&output.stderr);
    if !errors.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(errors.trim_end());
    }
    // `run` passes on the command's exit code.
    if tool == "imkr_run"
        && let Some(code) = output.status.code()
    {
        text.push_str(&format!("\n[exit code {code}]"));
        return (text, false);
    }
    (text, !output.status.success())
}

/// What a tool's output becomes: text, and for a screenshot the picture
/// itself as well. A path alone would be no use — the agent is the one
/// that has to look at it.
fn content(tool: &str, text: &str, failed: bool) -> Value {
    let mut blocks = vec![json!({ "type": "text", "text": text })];
    if tool == "imkr_screen_shot"
        && !failed
        && let Some(path) = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|shot| shot["path"].as_str().map(str::to_owned))
        && let Ok(bytes) = std::fs::read(&path)
    {
        blocks.push(json!({
            "type": "image",
            "mimeType": "image/png",
            "data": crate::clipboard::base64(&bytes),
        }));
    }
    Value::Array(blocks)
}

/// The answer to one JSON-RPC request, `None` for notifications.
fn handle(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": request["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "imkr", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "imkr runs this session's terminals. These tools find the other coding agents and panes, read them, type into them, message agents and run commands in other panes' shells. The imkr_screen tools see and drive the user's desktop, on this machine or another; ask imkr_screen_status first — it is off unless the machine's owner turned it on.",
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let params = &request["params"];
            let tool = params["name"].as_str().unwrap_or_default();
            let (text, failed) = call(tool, &params["arguments"]);
            json!({ "content": content(tool, &text, failed), "isError": failed })
        }
        method => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("no method {method}") },
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Serves requests from stdin until it closes.
pub fn serve() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle(&request),
            Err(e) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("not JSON: {e}") },
            })),
        };
        if let Some(answer) = answer {
            writeln!(out, "{answer}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_the_handshake_and_lists_tools() {
        let init = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        let list = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"imkr_run") && names.contains(&"imkr_message"));
        let unknown = handle(&json!({"jsonrpc":"2.0","id":3,"method":"nope"})).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn tools_become_command_lines_with_text_on_stdin() {
        let (line, input) =
            command_for("imkr_send", &json!({"pane": 3, "text": "ls; rm -rf /"})).unwrap();
        assert_eq!(line, ["send", "--stdin", "--pane", "3"]);
        assert_eq!(input, "ls; rm -rf /");
        let (line, _) = command_for(
            "imkr_run",
            &json!({"pane": 2, "command": "cargo test", "timeout": 60}),
        )
        .unwrap();
        assert_eq!(
            line,
            ["run", "--pane", "2", "--timeout", "60", "--", "cargo test"]
        );
        assert!(command_for("imkr_read", &json!({})).is_err());
    }
}
