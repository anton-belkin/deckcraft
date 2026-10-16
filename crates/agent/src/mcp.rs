//! The agent's MCP surface: JSON-RPC 2.0 (MCP `2025-06-18`) whose tools map onto `agent.*`
//! commands of the running app. Every read is masked (client data shows as pseudonyms); there is
//! no tool that returns real client text. The tool names match GridCraft's and WordCraft's agent
//! layers, so one agent skill serves all three.

use deckcraft_mcp::Backend;
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "You are connected to the DeckCraft presentation the user has open, through its agent layer. \
The user is preparing to present or sign off on this deck; your job is to make that fast. They ask questions on slides, shapes \
and numbers: watch for them (GET http://127.0.0.1:<port>/events streams one line per new question, or call wait_for_questions), \
claim_question, investigate with get_context / read_slides / list_links, then answer_question with links to the slides and \
sources you used (href \"slide:3\", \"shape:3/12\" = slide 3, shape id 12, \"note:n3\", or https URLs). For each slide, prepare \
answers to the questions the audience is likely to ask: add_note with kind \"faq\", the question as title, the answer as text \
(the presenter sees them in Presenter View). Leave explanations, assumptions, sources and checks with add_note too, and log \
decisions (log_decision). Change the deck only with run_command, giving a reason and a source. Linked tables and pictures come \
from spreadsheet cells: list_links shows their source and whether the numbers there changed since they were pasted. PRIVACY: \
text the user marked as client data shows as pseudonyms like [client-3] wherever it appears; the same text always gets the \
same pseudonym. Never present a pseudonym as real, never guess what it stands for, and never write it into the deck as if it \
were a name. You cannot change shapes holding client data. A question with kind \"ping\" is a connection check: answer it at once with \"here\", don't investigate. Questions belong to topics (list_topics, get_topic): link inside the text ([slide 4](slide:4), [3.2](place:3.2)); add places that a change here affects (add_places); leave notes that will still matter after the chat (add_note with the topic); never mark Agreed, propose it (propose_agreed). VOICE: ⟦n⟧ in a question is what the user pointed at while speaking (expanded as ⟦n: slide 2 › TextBox 3 “Revenue grew 6%”⟧), see refs; resolve \"this\", \"that one\", \"это\" from those and the context.";

fn tool(name: &str, desc: &str, props: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": {"type": "object", "properties": props, "required": required, "additionalProperties": false},
        "annotations": {"readOnlyHint": read_only, "destructiveHint": false, "openWorldHint": false},
    })
}

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}
fn n(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}
fn links() -> Value {
    json!({"type": "array", "description": "Links: {label, href, summary?}. href is https://…, slide:3, shape:3/12 (slide 3, shape id 12) or note:<id>", "items": {
        "type": "object", "properties": {"label": {"type": "string"}, "href": {"type": "string"}, "summary": {"type": "string"}}, "required": ["label", "href"]}})
}
fn target(props: &mut serde_json::Map<String, Value>) {
    props.insert("slide".into(), n("Slide number (1-based); default: the current slide"));
    props.insert("shape".into(), json!({"type": ["integer", "string"], "description": "Shape id (from read_slides) or name"}));
    props.insert("words".into(), s("Words in the shape's text the note is about"));
}

pub fn tools() -> Vec<Value> {
    let mut list = base_tools();
    for t in &mut list {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        if let Some(props) = t.pointer_mut("/inputSchema/properties").and_then(Value::as_object_mut) {
            if name != "list_documents" && name != "detach" && name != "list_commands" {
                props.insert("document".into(), s("The deck's id (from list_documents or attach); without it, the deck active in DeckCraft"));
            }
            if matches!(name.as_str(), "list_questions" | "wait_for_questions" | "claim_question" | "answer_question") {
                props.insert("listener".into(), s("Your listener id from attach: keeps this conversation listed as listening"));
            }
        }
    }
    list
}

fn base_tools() -> Vec<Value> {
    let mut note_props = serde_json::Map::new();
    note_props.insert("topic".into(), json!({"type": ["integer", "string"], "description": "The topic (its number or id); without it, the note starts a topic on its slide or shape (For information)"}));
    note_props.insert("text".into(), s("The note, with links inside the text: [slide 4](slide:4), [3.2](place:3.2) (for faq: the prepared answer)"));
    note_props.insert("title".into(), s("Short title (for faq: the question the audience may ask)"));
    note_props.insert("kind".into(), json!({"type": "string", "enum": ["explanation", "source", "assumption", "decision", "faq", "check"]}));
    note_props.insert("url".into(), s("For a source: its address"));
    note_props.insert("summary".into(), s("For a source: what is behind the link, in a line"));
    note_props.insert("links".into(), links());
    target(&mut note_props);
    let mut decision_props = serde_json::Map::new();
    decision_props.insert("text".into(), s("The decision and its reason"));
    decision_props.insert("source".into(), s("What it is based on"));
    target(&mut decision_props);
    vec![
        tool(
            "get_context",
            "Overview of the open deck: slides with titles, word counts, client data, open questions, prepared answers, linked cells, recent decisions.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "read_slides",
            "What is on slides: every shape with its id, kind, box, text, table cells, alt text and link; speaker notes; your notes. Client data shows as pseudonyms (flagged private).",
            json!({"slide": {"type": ["integer", "string"], "description": "Slide number (1-based) or \"all\"; default: the current slide"}}),
            &[],
            true,
        ),
        tool(
            "list_links",
            "Tables and pictures linked to spreadsheet cells: their source (workbook, sheet, range), when they were pasted, and whether the source changed since (state: current, stale, missing, error, unchecked).",
            json!({"check": {"type": "boolean", "description": "Check the sources now (default false: last known state)"}}),
            &[],
            true,
        ),
        tool(
            "list_questions",
            "Questions the user asked on the deck. status: pending (new), working (claimed), answered, open (pending+working) or all. A spoken question carries ⟦n: slide 2 › TextBox 3 “…”⟧ where the user pointed at a shape or slide while speaking; refs lists them.",
            json!({"status": {"type": "string", "enum": ["pending", "working", "answered", "open", "all"]}}),
            &[],
            true,
        ),
        tool(
            "wait_for_questions",
            "Block until there is a pending question (or the timeout passes) and return the pending ones. An alternative to watching /events.",
            json!({"timeout_seconds": n("Up to 50 (default 25)")}),
            &[],
            true,
        ),
        tool(
            "claim_question",
            "Take a pending question so the user sees it is being worked on.",
            json!({"id": s("Question id, e.g. \"q3\"")}),
            &["id"],
            false,
        ),
        tool(
            "answer_question",
            "Answer a question; the shape shows \"answer ready\". Link to the slides and sources you used.",
            json!({"id": s("Question id"), "text": s("The answer (plain text, short paragraphs)"), "links": links()}),
            &["id", "text"],
            false,
        ),
        tool(
            "add_note",
            "Leave a note in a topic that will still matter after the chat: why it is done this way (explanation), a source (url + summary), an assumption, a decision; or start a topic on a slide or shape with a prepared answer to a question the audience is likely to ask (kind faq: title = the question).",
            Value::Object(note_props),
            &["text"],
            false,
        ),
        tool(
            "list_topics",
            "The deck's topics in deck order: number, status and its text, places (where they are now), note titles, unread answers; plus readiness for sign-off.",
            json!({"status": {"type": "string", "enum": ["open", "revise", "agreed", "info"]}}),
            &[],
            true,
        ),
        tool(
            "get_topic",
            "Everything about a topic: places, status, notes, the discussion.",
            json!({"topic": json!({"type": ["integer", "string"], "description": "Its number or id"})}),
            &["topic"],
            true,
        ),
        tool(
            "add_places",
            "Attach places a change here affects to the same topic, each with a short reason (\"depends on 3.1: same revenue figure\"), so the dependency shows on the deck.",
            json!({"topic": json!({"type": ["integer", "string"]}), "places": {"type": "array", "items": {"type": "object", "properties": {"slide": n("Slide number"), "shape": json!({"type": ["integer", "string"], "description": "Shape id or name"}), "shapes": {"type": "array", "items": {"type": ["integer", "string"]}}, "words": s("Words in the shape's text"), "reason": s("Why this place belongs to the topic")}, "required": ["slide"]}}}),
            &["topic", "places"],
            false,
        ),
        tool(
            "widen_place",
            "Widen a place to what the answer is really about (the whole table, several shapes, the slide), and say why.",
            json!({"place": s("The place: its number (3.2) or id"), "to": {"type": "object", "properties": {"slide": n("Slide number"), "shape": json!({"type": ["integer", "string"]}), "shapes": {"type": "array", "items": {"type": ["integer", "string"]}}, "words": s("Words")}, "required": ["slide"]}, "reason": s("Why")}),
            &["place", "to"],
            false,
        ),
        tool(
            "set_status",
            "Set a topic's status with its text: open (a question, no decision), revise (something here must change), info (nothing to decide). Agreed is the person's: use propose_agreed.",
            json!({"topic": json!({"type": ["integer", "string"]}), "status": {"type": "string", "enum": ["open", "revise", "info"]}, "text": s("Why, with links inside the text")}),
            &["topic", "status"],
            false,
        ),
        tool(
            "propose_agreed",
            "Suggest that a topic is settled, with your reasons; the person confirms with one click.",
            json!({"topic": json!({"type": ["integer", "string"]}), "reasons": s("Why it can be agreed, with links")}),
            &["topic", "reasons"],
            false,
        ),
        tool("log_decision", "Record a decision in the deck's history (why it is the way it is).", Value::Object(decision_props), &["text"], false),
        tool("get_layer", "The whole agent layer: anchors, questions, notes, client-data marks, history.", json!({}), &[], true),
        tool(
            "run_command",
            "Change the deck with any DeckCraft command (text.insert, format.bold, shape.insert, slide.new…; see list_commands). Logged as yours with the reason and source. Reading commands, files and the clipboard are refused (use read_slides), and so is anything touching client data.",
            json!({"command": s("Command id"), "params": {"type": "object"}, "reason": s("Why"), "source": s("What it is based on")}),
            &["command", "reason"],
            false,
        ),
        tool(
            "list_commands",
            "List DeckCraft commands that change the deck (id, label, params).",
            json!({"search": s("Substring to filter on")}),
            &[],
            true,
        ),
        tool(
            "list_documents",
            "The decks open in DeckCraft: id, title, path, which is in front, open questions, who listens. Pass an id as `document` to any tool to work on that deck.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "attach",
            "Listen to a deck from this conversation: questions asked in it are for you. Returns your listener id: pass it (and the document) to list_questions, wait_for_questions, claim_question and answer_question.",
            json!({"label": s("What the person sees: the client and project, e.g. \"Cowork · Acme\"")}),
            &[],
            false,
        ),
        tool("detach", "Stop listening to a deck.", json!({"listener": s("Your listener id from attach")}), &["listener"], false),
    ]
}

/// Runs an `agent.*` command as the agent, on the deck `scope` names (AItoCraft's `document`)
/// and refreshing its `listener`.
fn exec(b: &mut dyn Backend, scope: &Value, command: &str, mut params: Value) -> Result<Value, String> {
    if let Value::Object(o) = &mut params {
        o.insert("actor".into(), json!("agent"));
        o.insert("masked".into(), json!(true));
        if let Some(sc) = scope.as_object() {
            for (k, v) in sc {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    b.call("engine.execute", json!({"command": command, "params": params}))
}

fn pick(args: &Value, keys: &[&str]) -> Value {
    let mut o = serde_json::Map::new();
    for k in keys {
        if let Some(v) = args.get(*k) {
            o.insert((*k).to_string(), v.clone());
        }
    }
    Value::Object(o)
}

pub fn call(b: &mut dyn Backend, name: &str, args: &Value) -> Result<Value, String> {
    let scope = pick(args, &["document", "listener"]);
    match name {
        "list_documents" => exec(b, &json!({}), "agent.documents", json!({})),
        "attach" => exec(b, &pick(args, &["document"]), "agent.attach", pick(args, &["label"])),
        "detach" => exec(b, &json!({}), "agent.detach", pick(args, &["listener"])),
        "get_context" => exec(b, &scope, "agent.context", json!({})),
        "read_slides" => exec(b, &scope, "agent.read", pick(args, &["slide"])),
        "list_links" => {
            if args.get("check").and_then(Value::as_bool) == Some(true) {
                exec(b, &scope, "link.check", json!({}))?;
            }
            exec(b, &scope, "link.list", json!({}))
        }
        "list_questions" => exec(b, &scope, "agent.questions", pick(args, &["status"])),
        "wait_for_questions" => {
            let secs = args.get("timeout_seconds").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(25.0).clamp(0.0, 50.0);
            let mut polls = (secs * 2.0) as u32;
            loop {
                // `waiting` tells the app an agent is listening (its Connection status).
                let v = exec(b, &scope, "agent.questions", json!({"status": "pending", "waiting": true}))?;
                if v.as_array().is_some_and(|a| !a.is_empty()) || polls == 0 {
                    return Ok(v);
                }
                polls -= 1;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        "claim_question" => exec(b, &scope, "agent.claim", pick(args, &["id"])),
        "answer_question" => exec(b, &scope, "agent.answer", pick(args, &["id", "text", "links"])),
        "add_note" => {
            exec(b, &scope, "agent.note", pick(args, &["topic", "text", "title", "kind", "url", "summary", "links", "slide", "shape", "words"]))
        }
        "list_topics" => exec(b, &scope, "agent.topics", pick(args, &["status"])),
        "get_topic" => exec(b, &scope, "agent.topic", pick(args, &["topic"])),
        "add_places" => exec(b, &scope, "agent.addPlaces", pick(args, &["topic", "places"])),
        "widen_place" => exec(b, &scope, "agent.widenPlace", pick(args, &["place", "to", "reason"])),
        "set_status" => exec(b, &scope, "agent.setStatus", pick(args, &["topic", "status", "text"])),
        "propose_agreed" => exec(b, &scope, "agent.proposeAgreed", pick(args, &["topic", "reasons"])),
        "log_decision" => exec(b, &scope, "agent.decision", pick(args, &["text", "source", "slide", "shape", "words"])),
        "get_layer" => exec(b, &scope, "agent.layer", json!({})),
        "run_command" => exec(b, &scope, "agent.exec", pick(args, &["command", "params", "reason", "source"])),
        "list_commands" => {
            let all = b.call("engine.commands", json!({}))?;
            let q = args.get("search").and_then(Value::as_str).unwrap_or("").to_lowercase();
            let list: Vec<Value> = all
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|c| {
                            let id = c.get("id").and_then(Value::as_str).unwrap_or("");
                            let label = c.get("label").and_then(Value::as_str).unwrap_or("");
                            !id.starts_with("agent.")
                                && !id.starts_with("link.")
                                && !id.starts_with("ui.")
                                && !id.starts_with("file.")
                                && (q.is_empty() || id.to_lowercase().contains(&q) || label.to_lowercase().contains(&q))
                        })
                        .map(|c| pick(c, &["id", "label", "params"]))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Value::Array(list))
        }
        other => Err(format!("unknown tool `{other}`")),
    }
}

/// Handles one JSON-RPC message; `None` for notifications.
pub fn handle(b: &mut dyn Backend, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = id?;
    let ok = |r: Value| json!({"jsonrpc": "2.0", "id": id, "result": r});
    let err = |code: i64, m: String| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}});
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
            let version = if SUPPORTED.contains(&asked) { asked } else { PROTOCOL_VERSION };
            ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}, "experimental": {"claude/channel": {}}},
                "serverInfo": {"name": "deckcraft-agent", "title": "DeckCraft agent layer", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            match call(b, name, &args) {
                Ok(v) => ok(json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}], "isError": false})),
                Err(e) => ok(json!({"content": [{"type": "text", "text": e}], "isError": true})),
            }
        }
        "resources/list" => ok(json!({"resources": []})),
        "prompts/list" => ok(json!({"prompts": []})),
        "logging/setLevel" => ok(json!({})),
        other => err(-32601, format!("method not found: {other}")),
    })
}
