# DeckCraft fork: the agent layer

This fork adds a layer of interaction with the presenter's own agent (Claude Code, Claude Desktop,
any MCP client) inside the presentation, so the person who presents or signs off a deck can do it
faster and safer: questions on slides, shapes and numbers answered in the background, prepared
answers to the audience's likely questions per slide (in Presenter View too), agent notes with
sources, a privacy gate for client data, a history of who changed what — and **tables and pictures
linked to spreadsheet cells** that know where their numbers come from and notice when they change.

The same layer exists in GridCraft (the reference design, `anton-belkin/gridcraft`, branch
`agent-layer`) and WordCraft; the MCP tool names are shared, so one agent skill pattern serves all
three. The fork is built to be **rebased onto upstream often and cleanly**: read "Keeping the fork
alive" before changing anything outside the agent crates.

## What is where

| Path | What | Upstream? |
| --- | --- | --- |
| `crates/agent/` (`deckcraft-agent`) | The layer: data model and storage in the `.pptx`, anchors, `agent.*` and `link.*` commands, privacy masking, Excel links (`links.rs`, `xlsx.rs`), the in-app MCP server and `/events` | fork only |
| `crates/agent-ui/` (`deckcraft-agent-ui`) | The **Agent** ribbon tab (`ribbon.rs`), badges on shapes and slides, the Agent pane, the linked-cells hover card, prepared answers in Presenter View, context-menu items, the background link checker; `examples/agent_snapshot.rs` renders it offscreen | fork only |
| `apps/deckcraft/src/agent.rs` | Starts the layer in the desktop app (`--agent-port`, `--no-agent`), and `--agent-stdio [--control HOST:PORT]` (the MCP over stdio for Claude Desktop and Cowork, no window) | fork only |
| `agent-layer/links.md` | **The Excel → slide link contract** (clipboard payload, staleness, back-references) and the exact list of what GridCraft adds | fork only |
| `agent-layer/skill/deckcraft-agent/` | The agent's skill | fork only |
| `agent-layer/rebase.sh` | Rebase onto upstream and run the gates | fork only |
| `crates/engine/src/ext.rs` | **Hook**: extension commands, command observers, command interceptors | new file in an upstream crate |
| `crates/ui-egui/src/ext.rs` | **Hook**: UI extensions (ribbon tabs, side panel, slide overlay, context menu, presenter notes, per-frame) | new file in an upstream crate |
| lines marked `fork:agent` | The only edits to upstream files | patch |

### The patch to upstream files

Every edit to a file that exists upstream carries a `fork:agent` comment.
`git grep -n fork:agent -- ':!crates/agent*' ':!agent-layer'` lists them: 30 lines, all insertions
or one-line changes:

- `crates/engine/src/lib.rs`: `pub mod ext;`; in `Session::execute` the observers before and after
  the command (4 lines); in `execute_unguarded` the command may be taken over by an interceptor
  (1 line); `Session::commands` lists extension commands (1 line).
- `crates/engine/src/cmd/mod.rs`: `find_command` also finds extension commands (1 line).
- `crates/ui-egui/src/lib.rs`: `pub mod ext;`, `ext::frame` in `SlideApp::ui`, `ext::side_panels`
  beside the task pane.
- `crates/ui-egui/src/canvas.rs`: `ext::paint_slide` after the built-in overlays, `ext::context_menu`
  at the end of the slide's context menu.
- `crates/ui-egui/src/show.rs`: `ext::presenter_notes` under the speaker notes in Presenter View.
- `crates/ui-egui/src/ribbon.rs`: extension tabs (`ext::ribbon_tabs`) known and listed after the
  built-in tabs, their content drawn by `ext::ribbon_tab` (5 lines).
- `apps/deckcraft/src/main.rs` (4 lines: `--agent-stdio` on the first line of `main`), `apps/deckcraft/Cargo.toml` (2): wiring.
- `Cargo.toml`: the two workspace dependencies.
- `xtask/src/layers.rs`: the two crates in the layer table (L6) and their order within L6.

The hooks know nothing about agents: they are generic extension points, written to be proposed
upstream. Each one accepted upstream removes its lines from the patch.

## Design decisions (and why)

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
  (mark/unmark the selection), *Connection* (the server, whether an agent is connected and who,
  Copy MCP Command, Connect Cowork) and *Links* (out-of-date count, Check Now, Update Stale). The
  pane keeps what is about the content: this slide's questions, prepared answers, notes and linked
  cells, all questions, link details, the history. Under the big buttons a status line says where
  each stands (`1 ready` / `2 waiting` / `1 in work` / `idle`; Cowork `connected` / `not set up` /
  `quit Claude (⌘Q)` / `open Claude` / `other build`; links `up to date` / `1 out of date`). The
  floating status button shows only while the ribbon is collapsed.
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
- **Prepared answers are notes of kind `faq` on a slide** — what the agent expects the audience to
  ask, with the answer. The presenter sees them in the pane and in Presenter View.
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
with ⌘Q and reopen it. Offscreen render with a demo deck (questions,
prepared answers, client data, a stale linked table):

```bash
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --pane slide out.png
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --hover-link out.png
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --presenter out.png
cargo run -p deckcraft-agent-ui --example agent_snapshot -- --agent-tab --pane slide out.png
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

Prototype. Works and tested: questions with live badges (waiting / working / answer ready), the
in-app MCP server and `/events`, prepared answers per slide (pane and Presenter View), agent notes
with links, client-data masking in every agent read (tested against leaks through every tool),
refusal of agent writes to client data, decision history and audit trail in the `.pptx`, anchors
that survive a `.pptx` round trip and slide moves, linked tables and pictures with stale detection
against a real `.xlsx` (anchor following inserted rows, file moved next to the deck, GridCraft
asked first), Update from Source as one undo step.

Waiting on GridCraft (see `agent-layer/links.md`, "What GridCraft adds"): Copy Link to Cells,
`link.read`, `link.addUses` and the "Used in" back-reference, `link.reveal`, `--goto`. Until then a
linked paste can be produced by any app putting the payload on the clipboard, and DeckCraft checks
links against the `.xlsx` file.
