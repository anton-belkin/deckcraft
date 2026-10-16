# DeckCraft fork: the agent layer

This fork adds a layer of interaction with the presenter's own agent (Claude Code, Claude Desktop,
any MCP client) inside the presentation, so the person who presents or signs off a deck can do it
faster and safer: **topics** (`~/aitocraft/TOPICS.md`) — questions on slides, shapes and numbers,
answered in the background, each with its places, a status (Open, Revise, Agreed, For information),
notes and a discussion, marked on the slide by small numbered circles —, prepared answers to the
audience's likely questions per slide (in Presenter View too), readiness for sign-off, a privacy
gate for client data, a history of who changed what, everything the person does undoable — and **tables and pictures
linked to spreadsheet cells** that know where their numbers come from and notice when they change.

The same layer exists in GridCraft (the reference design, `anton-belkin/gridcraft`, branch
`agent-layer`) and WordCraft; the MCP tool names are shared, so one agent skill pattern serves all
three. The fork is built to be **rebased onto upstream often and cleanly**: read "Keeping the fork
alive" before changing anything outside the agent crates.

## What is where

| Path | What | Upstream? |
| --- | --- | --- |
| `crates/agent/` (`deckcraft-agent`) | The layer: data model (v2) and storage in the `.pptx`, anchors, topics (`topics.rs`: numbering, places, readiness; `topic_cmds.rs`; `textlinks.rs`: links inside text; `undo.rs`: the person's undo steps), `agent.*` and `link.*` commands, privacy masking, Excel links (`links.rs`, `xlsx.rs`), the in-app MCP server and `/events` | fork only |
| `crates/agent-ui/` (`deckcraft-agent-ui`) | The **Agent** ribbon tab (`ribbon.rs`), topic markers on shapes and slides, the Agent pane (Selection, Overview, History), the linked-cells hover card, prepared answers and the markers toggle in Presenter View, context-menu items, the background link checker; `examples/agent_snapshot.rs` renders it offscreen | fork only |
| `apps/deckcraft/src/agent.rs` | Starts the layer in the desktop app (`--agent-port`, `--no-agent`), and `--agent-stdio [--control HOST:PORT]` (the MCP over stdio for Claude Desktop and Cowork, no window) | fork only |
| `agent-layer/links.md` | **The Excel → slide link contract** (clipboard payload, staleness, back-references) and the exact list of what GridCraft adds | fork only |
| `agent-layer/skill/deckcraft-agent/` | The agent's skill | fork only |
| `agent-layer/rebase.sh` | Rebase onto upstream and run the gates | fork only |
| `crates/engine/src/ext.rs` | **Hook**: extension commands, command observers, command interceptors, custom undo steps (`push_step`) | new file in an upstream crate |
| `crates/ui-egui/src/ext.rs` | **Hook**: UI extensions (ribbon tabs, side panel, slide overlay, context menu, presenter notes and slide overlay, per-frame) | new file in an upstream crate |
| lines marked `fork:agent` | The only edits to upstream files | patch |

### The patch to upstream files

Every edit to a file that exists upstream carries a `fork:agent` comment.
`git grep -n fork:agent -- ':!crates/agent*' ':!agent-layer'` lists them: 31 lines, all insertions
or one-line changes:

- `crates/engine/src/lib.rs`: `pub mod ext;`; in `Session::execute` the observers before and after
  the command (4 lines); in `execute_unguarded` the command may be taken over by an interceptor
  (1 line); `Session::commands` lists extension commands (1 line).
- `crates/engine/src/cmd/mod.rs`: `find_command` also finds extension commands (1 line).
- `crates/ui-egui/src/lib.rs`: `pub mod ext;`, `ext::frame` in `SlideApp::ui`, `ext::side_panels`
  beside the task pane.
- `crates/ui-egui/src/canvas.rs`: `ext::paint_slide` after the built-in overlays, `ext::context_menu`
  at the end of the slide's context menu.
- `crates/ui-egui/src/show.rs`: `ext::presenter_notes` under the speaker notes in Presenter View,
  `ext::presenter_slide` over the presenter's picture of the current slide.
- `crates/ui-egui/src/ribbon.rs`: extension tabs (`ext::ribbon_tabs`) known and listed after the
  built-in tabs, their content drawn by `ext::ribbon_tab` (5 lines).
- `apps/deckcraft/src/main.rs` (4 lines: `--agent-stdio` on the first line of `main`), `apps/deckcraft/Cargo.toml` (2): wiring.
- `Cargo.toml`: the two workspace dependencies.
- `xtask/src/layers.rs`: the two crates in the layer table (L6) and their order within L6.

The hooks know nothing about agents: they are generic extension points, written to be proposed
upstream. Each one accepted upstream removes its lines from the patch.

## Design decisions (and why)

- **Topics** (`~/aitocraft/TOPICS.md`, shared with GridCraft and WordCraft). A question starts a
  topic on the selection, or joins the topic the selection already has ("New Topic" starts a
  separate one). A topic has places (shapes by their hidden key, words in a shape, slides), a
  status with a line of text, notes and the discussion. Numbering is deck order: slides in order,
  then on a slide its own places first, then shapes by rows of 24 pt top to bottom and left to
  right (z-order isn't a reading order), then words by their offset in the text; places of a
  topic with several are 3.1, 3.2. The marker is a small circle with the number at the shape's
  top-right corner (the slide's own at the slide's), in the status colour, blue and pulsing while
  the agent works on it, with a dot for an unread answer; several stack leftwards. In Presenter
  View they show only when the presenter ticks "Show topic markers" under the notes. Only a person
  sets Agreed; the agent proposes it and the card has Confirm Agreed. A linked table whose source
  changed raises its topics to Revise (by DeckCraft, in the log), once per change.
- **Links inside text are anchors.** `[slide 4](slide:4)`, `shape:`, `place:`, `topic:`, `note:`
  and a plain "slide 4" are stored as `anchor:<id>` (or topic and place ids) when an answer, note,
  status text or question is stored, and shown with the current address: insert a slide before it
  and the answer says "slide 5". A label that was the address follows; a written label stays.
- **Deleted shapes keep their whereabouts.** Every anchor remembers where its shape was ("on slide
  3, after Content Placeholder 2"), refreshed as the deck changes, so a place whose shape is
  deleted reads "removed — was on slide 3, after …", struck through; undo of the delete brings it
  back. Duplicating a shape or a slide duplicates its places into the same topics (the copy gets a
  fresh hidden key and a new place).
- **The person's actions are undo steps** in the deck's ordinary history (engine hook
  `ext::push_step`: the entry holds an identical copy of the presentation, recognised by pointer
  after `edit.undo`/`edit.redo`, so a layer step never changes slides and a slide step never
  touches the layer): ask, new topic, places, status, reject, pin, remove note, client data. Undo
  of a question not yet taken hides it; being worked on, withdraws it (the agent gets a `withdrawn`
  event and its answer is refused with "this question was withdrawn by the user; drop it");
  answered, hides the topic with its answer. Redo re-sends, or restores without asking again. The
  agent's work is not on the person's stack: each piece has Remove or Reject, which is.
- **The layer format is v2**; v1 decks are migrated on load (one topic per question, notes on an
  anchor become a For-information topic). **A copy to send** (Overview ▸ Save a Copy to Send…,
  `agent.copyToSend`) keeps statuses, places and notes and drops the discussion unless the person
  ticks "Include the discussion".

- **Commands, not side channels.** Everything the layer does is an `agent.*` or `link.*` command
  registered through `deckcraft_engine::ext`, so the UI, the control channel, the CLI and MCP reach
  it like any other command, and `Session::execute` keeps its panic guard around it.
- **The layer lives in a per-document store, not in the undo history.** Undoing a slide edit must
  never take away a question or an answer. It is written into the presentation (a custom XML part,
  `urn:deckcraft:agent-layer:v1`, JSON in CDATA, `customXml/deckcraftAgent.xml`) just before a
  save, by an observer on `file.save*`. DeckCraft and PowerPoint keep custom XML parts, so the
  layer survives a round trip through PowerPoint.
- **Anchors are hidden keys in tags** (`DECKCRAFT_AGENT_KEY` in the slide's and the shape's
  `p:custDataLst`), the deck's equivalent of GridCraft's hidden defined names: they survive the
  renumbering of ids on open, slide moves, copy and paste, and PowerPoint. An anchor is a slide, a
  shape, words in the shape's text, or a table cell. The last known slide and shape ids are kept
  too: an undo that takes a key away gets it back.
- **Privacy is enforced where the agent reads.** No MCP tool returns real client text: every read
  goes through a masker that replaces marked text with stable pseudonyms (`[client-3]`). A whole
  marked shape hides everything in it, and its names (runs of capitalised words) and figures (3+
  digits) are masked wherever else they appear in the deck; marked words are masked everywhere.
  Pseudonyms are numbered by a hash of the text, kept in the layer, so they stay the same across
  sessions without storing the text twice. The agent can't change a marked shape (by id, by
  selection, or by deleting or rearranging its slide) and can't lift a mark. Reads through
  `run_command`, files and the clipboard are refused. Not covered: the control port (7985) and the
  `.pptx` on disk hold real text (same as GridCraft; the "safe file" mode is the next step).
- **The Agent tab holds what isn't about one piece of content**, in four groups named the same in
  GridCraft, WordCraft and DeckCraft: *Agent* (the pane, with what is waiting), *Client data*
  (mark/unmark the selection), *Connection* and *Links* (out-of-date count, Check Now, Update
  Stale). The pane keeps what is about the content, in three tabs: **Selection** (what is selected,
  the topics touching it as cards, the question box at the very bottom), **Overview** (readiness
  "Agreed 10 of 15 · 2 revise · 3 open" on top, filters, every topic in deck order, the linked
  cells, a copy to send) and **History**. Under the Agent button the readiness line (`Agreed 3 of 5
  · 1 new`); links `up to date` / `1 out of date`. The floating status button shows only while the
  ribbon is collapsed.
- **Connection: three things only** (the shared spec). *Status*, with **Check** next to it, which
  asks the agent "Is anyone there?" (`agent.ping`): a system question (`kind: "ping"`) that goes
  out through every path a question goes (list_questions, wait_for_questions, `/events`, the
  channel, the background agent) but is never shown, gets no badge and is never saved; any answer
  marks it answered. Green "Agent answering · checked 12 s ago" after an answer; amber "Listener
  connected · not checked" on passive signs only (an `/events` stream, a channel stream, a
  `wait_for_questions` open now or in the last minute, the background agent running); grey "Nobody
  is listening"; red "No answer to check" a minute after an unanswered check. The sub-line says
  which signs and when the agent last called. *Mode*, a list with one line of explanation each:
  I connect my agent (MCP: Claude Code, Cowork, any client), Send to my Claude Code session
  (Channels), Background agent (DeckCraft runs claude itself), Off (the agent's calls are refused).
  *Copy ▾*, whose items depend on the mode, each copying one thing: Claude Code: add MCP server,
  Monitor command, Connect Cowork… / 1. Add channel server, 2. Start Claude Code with the channel /
  for the background agent, Open session in Claude Desktop and New session instead. URL, ports,
  the session and the last error are under Details. The mode is the app's (remembered in
  `<settings>/agent/mode`; `--agent-mode` overrides it), never the file's: a deck someone sends must not start an
  agent that spends the reader's money.
- **AItoCraft** (`~/aitocraft/PROTOCOL.md`): one MCP hub, configured once in Claude Desktop /
  Cowork and Claude Code, serves every open document of every Craft app; the person picks the
  context by starting the conversation where the work belongs and saying which deck. DeckCraft's
  side: the registry file `ArtCraft/agents/deckcraft-<pid>.json` (`registry.rs`: atomic writes,
  at most once a second on change, a 10 s heartbeat, deleted when the window closes) lists the open
  decks (`deckcraft:<pid>:<doc uid>`, title, path, frontmost, `focusedAt`, open questions,
  listeners; no content). Every tool takes `document` and works on a deck that isn't active (the observer makes
  it active for the command, so its undo step lands there, and puts the active deck back; an
  unknown id is "document … is not open in DeckCraft"). `list_documents`, `attach {document,
  label}` → a listener, `detach`; `listener` on the question tools refreshes it (dropped after
  90 s); questions carry `document` and `documentTitle`; `GET /events?document=<id>` follows one
  deck; a check goes to the active deck's listeners and records who answered. The Connection
  status is per deck: "Listening: Cowork · Acme · 20 s ago", the check, or "Nobody is listening for
  this document" (questions still queue). A check's id is `ping<ms>`; unanswered, it drops out of
  `agent.questions` after 60 s (the status still says "No answer to check"). `wait_for_questions`
  sends `waiting: true` (a listener for 60 s), the app's own polls (`/events`, the channel, the
  runner) `poll: true`; neither counts as the agent's last call. In Off, every agent-side command
  is refused with "the agent connection is off in DeckCraft (Agent tab ▸ Connection ▸ Mode)";
  `initialize` and `tools/list` still answer. Modes, in order: Claude Desktop / Cowork (AItoCraft),
  My Claude Code session (AItoCraft channel), Background agent, Off. Copy: Copy phrase for Claude,
  Install AItoCraft in Claude Desktop (`aitocraft --install-desktop`, when the binary is found:
  `AITOCRAFT_BIN`, the PATH, `~/aitocraft/target/release`), Claude Code: add AItoCraft (and, in
  the channel mode, start with the channel). The direct connection (`--agent-stdio`, HTTP, the
  monitor command, Connect Cowork with DeckCraft's own entry) stays under Details.
- **Send to my Claude Code session** (Claude Code Channels, research preview): the server declares
  `experimental["claude/channel"]` and, in that mode, streams each new question on `GET /mcp`
  (SSE) as `notifications/claude/channel`. Claude Code spawns channel servers over stdio, so
  `deckcraft --agent-stdio` bridges: requests through the control channel, events from the app's
  `GET /mcp` to stdout. `claude mcp add deckcraft-channel -- <deckcraft> --agent-stdio --control
  127.0.0.1:7985 --agent-port 7984`, then `claude --dangerously-load-development-channels
  server:deckcraft-channel`. Interactive sessions only (`-p` ignores channels).
- **Background agent** (`crates/agent/src/runner.rs`, demoted to the third mode, PROTOCOL §4): one
  `claude -p --input-format stream-json --output-format stream-json --verbose` process per deck,
  started by the first open question, in a neutral, empty folder (`ArtCraft/agent-sessions/`) so no
  project's CLAUDE.md, memory or settings become its context; the deck's folder only when the
  person ticks "Use the deck's folder as its project". The mode's line says "The app runs Claude on
  your subscription. It sees only this document. Uses your limits." With
  `--strict-mcp-config` and only the DeckCraft server, `--allowedTools "mcp__deckcraft__*"`, the
  skill as `--append-system-prompt-file`, `--max-turns 40`, `--max-budget-usd 3`. It is named
  `--name "DeckCraft · <file>"`, gets each new question once as a stream-json user message, and its
  session id is kept in the layer: reopening the deck resumes it (`--resume`); a resume that fails
  here, or New session, starts a fresh one from a summary of the history. Stop is end of input,
  then SIGINT, then kill. Never `--bare`. `DECKCRAFT_AGENT_COMMAND` replaces `claude`; tests and the
  snapshot example use a command that doesn't exist.
- **Cowork over stdio** (the contract GridCraft and WordCraft share). Claude Desktop and Cowork
  start local MCP servers as processes: `deckcraft --agent-stdio [--control 127.0.0.1:7985]` is
  handled on the first line of `main`, before logging and the window, speaks newline-delimited
  JSON-RPC on stdin/stdout (nothing else on stdout) and exits when stdin closes. It reaches the
  running app through its control channel, connecting on the first tool call, so Claude can start
  before DeckCraft; while DeckCraft is closed, tools say "… not reachable … Is DeckCraft open?" and
  `initialize`/`tools/list` still work. **Connect Cowork** writes
  `"deckcraft": {"command": <this executable>, "args": ["--agent-stdio", "--control", "127.0.0.1:<port>"]}`
  into Claude Desktop's `claude_desktop_config.json`: a backup first (`.bak-deckcraft-<ms>`), only
  `mcpServers.deckcraft` changed, a file that isn't a JSON object never touched, written through a
  temp file and a rename. Claude Desktop rewrites that file from memory when it quits, so when it
  is running DeckCraft waits in the background (`ps -axo comm=`, the main
  `/Claude.app/Contents/MacOS/Claude` only) and writes 1.5 s after it has quit; the button's status
  says "quit Claude (⌘Q)", then "open Claude". In tests the config path is never the real one and
  Claude counts as running. There is no event stream over stdio: the agent listens with
  `wait_for_questions` in a loop. "Agent active" on the tab comes from the agent's own calls to the
  layer (any transport) in the last two minutes.
- **Keytip A** (Alt, A; F10, A on macOS) opens the Agent tab: DeckCraft has no keytip system of its
  own, so the extension handles the sequence before the app reads the keyboard (the A isn't typed
  into the slide).
- **Prepared answers are notes of kind `faq` on a slide** (a For-information topic) — what the
  agent expects the audience to ask, with the answer. The presenter sees them in the topic's card
  and in Presenter View.
- **The MCP server runs inside the app** (`http://127.0.0.1:7984/mcp`, Streamable HTTP, JSON
  responses) and reaches the deck through the app's own loopback control channel (7985 by default
  with the layer on; GridCraft keeps 7979, which the links ask). `/events` streams new questions,
  one JSON line each, for an agent's background monitor (`curl -sN …/events`).
- **Excel links** (spec: `agent-layer/links.md`). A linked copy from GridCraft carries a JSON
  payload inside its clipboard HTML; pasting it makes a native table (or, Paste Linked as Picture,
  a picture drawn by DeckCraft's renderer) that keeps the source on the shape's tags. A plain
  Ctrl+V of such HTML is taken over by an interceptor on `edit.paste`, so it is one undo step.
  Staleness: a running GridCraft is asked first (it knows unsaved numbers); otherwise the `.xlsx`
  is read directly through the hidden anchor name, so inserted rows don't break the link. The file
  is looked for at its path and next to the deck. Update keeps the presenter's table formatting
  and formats new numbers like the old ones. Uses go to a registry file and to a running GridCraft
  for its "Used in" back-reference; `link.goto` brings the deck to the shape.

## Running it

```bash
cargo run --release -p deckcraft -- --sample
```

The agent server starts with the app (and the control channel on 7985 with it). Connect Claude Code:

```bash
claude mcp add --transport http deckcraft http://127.0.0.1:7984/mcp
```

and give it the skill in `agent-layer/skill/`. **Claude Desktop / Cowork**: Agent ▸ Connect Cowork
(or add the entry from `agent-layer/claude_desktop_config.example.json` by hand), then quit Claude
with ⌘Q and reopen it. Offscreen render with a demo deck (topics in every status, a removed
place, prepared answers, client data, a stale linked table):

```bash
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --pane selection out.png
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --pane overview out.png
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --pane topic:8 --slide 3 out.png
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --hover-link out.png
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --presenter out.png
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --agent-tab --pane selection out.png
```

## Keeping the fork alive

```bash
agent-layer/rebase.sh
```

The script fetches `upstream`, rebases the current branch onto `upstream/main` (taking upstream's
`Cargo.lock` and letting cargo add the two crates when the lock conflicts), and checks the result:
every `fork:agent` marker is still there, every hook is still called, then fmt, clippy, the tests of
the engine, UI, MCP, app and agent crates, layers, assets, wasm and a release build. When it stops
on a conflict, the conflict can only be in one of the marked lines above: keep upstream's version,
put the marked line back, `git rebase --continue`, then `agent-layer/rebase.sh --check`.

Rules that keep it this way:

1. New behaviour goes in `crates/agent*` or behind a hook, never into an upstream file.
2. Need a new hook? Make it generic (no agent vocabulary), put it in the crate's `ext.rs`, call it
   with one marked line, list it above.
3. Use public engine and UI APIs. When upstream renames one, the build breaks in `crates/agent*`
   with a clear error instead of a conflict.
4. Keep marked lines short enough that rustfmt leaves their neighbours alone.
5. Commits: hooks and wiring in their own commits, separate from the agent crates.

## Status

Prototype. Works and tested: topics (TOPICS.md v1: places, statuses, numbering in deck order,
markers, links inside text that follow the deck, removed places, duplicated places, undo of every
person action, only a person sets Agreed, v1 migration, a copy to send without the discussion; not
yet: nested places, TOPICS.md §6), the in-app MCP server and `/events`, prepared answers per slide
(pane and Presenter View), agent notes with links, client-data masking in every agent read (tested against leaks through every tool),
refusal of agent writes to client data, decision history and audit trail in the `.pptx`, anchors
that survive a `.pptx` round trip and slide moves, linked tables and pictures with stale detection
against a real `.xlsx` (anchor following inserted rows, file moved next to the deck, GridCraft
asked first), Update from Source as one undo step.

Waiting on GridCraft (see `agent-layer/links.md`, "What GridCraft adds"): Copy Link to Cells,
`link.read`, `link.addUses` and the "Used in" back-reference, `link.reveal`, `--goto`. Until then a
linked paste can be produced by any app putting the payload on the clipboard, and DeckCraft checks
links against the `.xlsx` file.
