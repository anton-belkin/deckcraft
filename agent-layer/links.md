# Excel → slide links: the contract (v1)

Cells copied in GridCraft and pasted on a DeckCraft slide stay linked to their source. The slide
says where its numbers come from, notices when they change in the workbook, and opens the
workbook at them; the workbook shows which decks use a range and jumps to the slide. PowerPoint
never had the second half.

This file is the contract between the two apps. DeckCraft's side is implemented
(`crates/agent/src/links.rs`, `crates/agent/src/xlsx.rs`); GridCraft's side is the list in
[What GridCraft adds](#what-gridcraft-adds).

## The two modes

| Mode | On the slide | In any other app (PowerPoint) | Update |
| --- | --- | --- | --- |
| `table` (default) | A native table of the values, formatted by DeckCraft (deck's table style, numbers right-aligned, header row) | A normal table | Only the words change; the presenter's formatting stays. A range that changed size is rebuilt |
| `picture` | A picture of the cells (from the payload's `image`, else drawn by DeckCraft's renderer) | A normal picture | Redrawn from the new values |

Both keep the link on the shape, so the stale check, hover card and Open Source work the same.
Ctrl+V of a linked copy pastes a table; the slide's context menu has **Paste Linked as Picture**.

## 1. The clipboard

A linked copy puts two flavours on the clipboard (one `set_html(html, plain)` call with arboard):

- **HTML**: a plain `<table>` of the displayed text, so every other app gets a normal table, with
  the payload in one attribute of the table element:

  ```html
  <table data-artcraft-link="eyJraW5kIjoiYXJ0Y3JhZnQubGluayIs…"><tr><td>Region</td>…</tr>…</table>
  ```

  The attribute value is the payload JSON, UTF-8, **standard base64 with padding** (whitespace in
  it is ignored). Readers find the first `data-artcraft-link=` in the HTML.
- **Plain text**: the displayed text, tab-separated, rows on lines (what Excel puts there too).

Where a platform clipboard can carry custom types, the same JSON may also go out as
`application/x-artcraft-link+json`. v1 readers only rely on the HTML flavour.

## 2. The payload

```json
{
  "kind": "artcraft.link",
  "version": 1,
  "source": {
    "app": "GridCraft",
    "path": "/Users/anton/Models/q3.xlsx",
    "workbookId": "6f1c0e4e-…",
    "anchor": "_GcLink_l7",
    "sheet": "Model",
    "range": "B4:D9",
    "label": "Revenue by region"
  },
  "values": [["Region", "Q2", "Q3"], ["North", 1180000, 1250000], ["South", 910000, null]],
  "text":   [["Region", "Q2", "Q3"], ["North", "1,180,000", "1,250,000"], ["South", "910,000", ""]],
  "copiedAt": 1792150000000,
  "private": false,
  "image": "iVBORw0KGgo…"
}
```

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `kind` | string | yes | Always `artcraft.link` |
| `version` | integer | yes | `1`. A reader refuses a version it doesn't know |
| `source.app` | string | yes | `GridCraft` |
| `source.path` | string | yes | Absolute path of the workbook **as saved**. An unsaved workbook can't be a linked source (GridCraft asks to save first) |
| `source.workbookId` | string | yes | The workbook's own stable id (see 4.1), so a moved or renamed file is recognised |
| `source.anchor` | string | yes | Hidden defined name on exactly the copied range (see 4.1) |
| `source.sheet` | string | yes | Sheet name at copy time (no quotes) |
| `source.range` | string | yes | A1 range at copy time, no `$`, no sheet (`B4:D9`, `C5`) |
| `source.label` | string | no | What the range is about, for people (a header, a named range's name, the sheet title) |
| `values` | array of rows | yes | One array per row, same width; each cell a number, string, boolean or `null` (empty). Error cells as their text (`"#DIV/0!"`). Numbers are the cell values, unformatted (a percentage is `0.42`) |
| `text` | array of rows | yes | The cells as displayed (number format applied): what the table shows |
| `copiedAt` | integer | yes | Milliseconds since the Unix epoch |
| `private` | boolean | no | Some copied cells are marked as client data in GridCraft: DeckCraft marks the pasted shape as client data for its agent |
| `image` | string | no | Base64 PNG of the cells as GridCraft draws them, used by `picture` mode. Without it DeckCraft draws the picture |

Limits a reader enforces: at most 75 rows, 75 columns and 2,000 cells (a slide table is for people
to read); a writer should refuse bigger copies with a clear message. Ragged rows are padded with
empty cells; a missing `text` cell is made from its value.

## 3. What DeckCraft keeps

The link lives on the pasted shape, in its tags (`p:custDataLst`, kept by PowerPoint and by copy
and paste between decks), tag `DECKCRAFT_LINK`, value JSON:

```json
{"v": 1, "id": "l18f…", "mode": "table", "source": { …as in the payload… },
 "values": [[…]], "text": [[…]], "copiedAt": 0, "pastedAt": 0, "updatedAt": 0, "private": false}
```

`values`/`text` are what the slide shows now (the baseline the check compares with). The shape
also gets a hidden key (tag `DECKCRAFT_AGENT_KEY`) that the workbook's back-reference uses to find
it again whatever happens to shape ids.

Commands (all reachable from the UI, the control channel and scripts):

| Command | Does |
| --- | --- |
| `edit.paste {text, html}` | A plain paste: HTML carrying a payload becomes a linked `table` (an extension takes the paste over) |
| `link.paste {payload \| html, mode?, rect?}` | Paste a payload as `table` or `picture` |
| `link.check {id?, gridcraft?}` | Check one or every link: `current`, `stale`, `missing`, `error` |
| `link.update {id?}` | Bring the source's current values in (one undo step) |
| `link.open {id?, dryRun?}` | Open the workbook at the linked cells |
| `link.list`, `link.unlink`, `link.uses`, `link.goto {link? \| shapeKey? \| anchor?}` | List, forget the source, registry lines, select the linked shape |

## 4. Checking for changes

DeckCraft checks every link when a deck opens, every 30 s while it is open, and on demand. One
check:

1. **Ask a running GridCraft** (it has the latest numbers, saved or not): JSON-lines call on its
   control channel, `127.0.0.1:7979` unless `GRIDCRAFT_CONTROL` says otherwise, 250 ms to connect,
   3 s to answer:

   ```json
   {"id":1,"method":"engine.execute","params":{"command":"link.read","params":{
     "path":"/Users/anton/Models/q3.xlsx","workbookId":"6f1c…","anchor":"_GcLink_l7","sheet":"Model","range":"B4:D9"}}}
   ```

   Answer (`result`), when GridCraft has that workbook open (by `workbookId`, else by path):

   ```json
   {"path":"/Users/anton/Models/q3.xlsx","sheet":"Model","range":"B5:D10",
    "values":[[…]],"text":[[…]]}
   ```

   `range` is where the anchor is now. An error (`ok: false`) or no answer means "not open there":
   go to step 2.
2. **Read the `.xlsx` directly** (no GridCraft needed): look for the file at `source.path`, then
   with the same file name in the deck's folder (a deck and its workbook moved together). Resolve
   `source.anchor` among the workbook's defined names; when the name is gone, fall back to
   `sheet`/`range`; when it is `#REF!`, the cells were deleted (`error`). Read the cached values
   (`<v>`) of the cells.
3. **Compare** with the shape's `values`: numbers equal within 1e-9 relative, strings trimmed,
   empty string = `null`. A different number of rows or columns is a change too.

States: `current`, `stale` (with the changed cells: old and new text), `missing` (the file is in
neither place), `error` (unreadable, cells deleted), `unchecked`.

On update, the new text is GridCraft's displayed text when it answered; from the file DeckCraft
formats the new value like the old text (prefix and suffix such as `$` and `%`, decimals,
thousands separators, parentheses for negatives).

### 4.1 Anchors and the workbook id (GridCraft)

- The anchor is a **hidden, workbook-scoped defined name** `_GcLink_<id>` on exactly the copied
  range (absolute reference, `Model!$B$4:$D$9`). Copying the same range again reuses its name.
  Like the agent layer's `_GcAgent_` names, the engine and Excel move it with inserted and deleted
  rows and columns; unlike them, link names are **never removed** by the agent layer's clean-up
  (other files depend on them). Deleting the cells leaves `#REF!`, which readers report.
- The workbook id is a UUID created the first time the workbook is a linked source and kept in
  the workbook (in the agent layer's custom XML part, `workbookId`), so it survives Save As, a move
  and a rename. A copy made with the file manager has the same id; that's fine, the path breaks
  the tie.

## 5. Back-references (the workbook knows where it is used)

A **registry of uses**, one JSON object per line, appended by DeckCraft whenever it pastes a link
and whenever it saves a deck with links (the deck's path is known then):

- macOS `~/Library/Application Support/ArtCraft/links.jsonl`, Windows `%APPDATA%\ArtCraft\links.jsonl`,
  Linux `$XDG_CONFIG_HOME/artcraft/links.jsonl` (or `~/.config/artcraft/`); `ARTCRAFT_LINKS_FILE`
  overrides.

```json
{"event":"use","at":1792150000000,"app":"DeckCraft","link":"l18f…",
 "workbook":"/Users/anton/Models/q3.xlsx","workbookId":"6f1c…","anchor":"_GcLink_l7","sheet":"Model","range":"B4:D9",
 "deck":"/Users/anton/Decks/board.pptx","deckTitle":"board","slide":3,"slideTitle":"Revenue","shape":412,"shapeKey":"k4f2a9","mode":"table"}
```

Readers keep the **latest line per `link`** and drop uses whose deck no longer exists or no longer
contains that link (check by opening the deck lazily, or just show the path). The file is trimmed
to its newest half past 8 MB.

DeckCraft also tells a running GridCraft right away (best effort):
`engine.execute {"command":"link.addUses","params":{"uses":[…the same objects…]}}`.

Jumping back: DeckCraft's control channel (`127.0.0.1:7985` when the agent layer is on, else the
`--control` port) takes `engine.execute {"command":"link.goto","params":{"link":"l18f…"}}` (or
`shapeKey`, or `anchor`) and selects that shape. When DeckCraft isn't running, open the deck
(`deckcraft board.pptx`) and send `link.goto` once it is up.

## 6. Opening the source (from the slide)

`link.open` on a linked shape:

1. GridCraft running: `engine.execute {"command":"link.reveal","params":{"path","workbookId","anchor","sheet","range"}}`
   (GridCraft opens the workbook if needed, activates the sheet, selects the anchored range and
   comes to the front).
2. Else, `GRIDCRAFT_BIN` set: `gridcraft <path> --goto <anchor>` (`--goto` takes a name or `Sheet!A1`).
3. Else, the system's default app for the file.

## 7. Ports

| App | Control channel | Agent MCP |
| --- | --- | --- |
| GridCraft | 7979 | 7980 |
| WordCraft | — | 7982 |
| DeckCraft | 7985 with the agent layer on (else `--control`) | 7984 |

## What GridCraft adds

Everything here is new in GridCraft, built the same way as its agent layer (commands registered
through `gridcraft_engine::ext`, UI through `gridcraft_ui_egui::ext`):

1. **`link.copy` — Copy as Linked Source** (ribbon Home ▸ Clipboard, cell context menu
   "Copy Link to Cells", Ctrl+Alt+C): refuses an unsaved workbook (asks to save) and ranges over
   75×75 / 2,000 cells; creates or reuses the hidden name `_GcLink_<id>` on the selection; creates
   the `workbookId` if missing; builds the payload (§2: values, displayed text, label from the
   header row or a named range, `private` when the range intersects client data, optionally a PNG
   of the cells); puts HTML + TSV on the clipboard (`services.clipboard_set_html`).
2. **Never drop `_GcLink_` names** in the agent layer's anchor clean-up, and keep them hidden in
   the Name Box (they already are, as `_GcAgent_` names).
3. **`link.read {path, workbookId, anchor, sheet, range}`** (query command): finds the open
   workbook by id, else path; resolves the anchor; returns `{path, sheet, range, values, text}` as
   in §4 step 1; errors when it doesn't have that workbook open.
4. **`link.addUses {uses}`** and the **"Used in" back-reference**: keep uses per anchor (from
   `link.addUses` and from the registry file on open, latest per `link`), store them in the agent
   layer's part (`uses`) so they travel with the workbook; show a badge on anchored ranges
   ("▸ 2 slides"), a hover listing deck, slide number and title, and a **Go to Slide** action that
   calls DeckCraft's `link.goto` (§5), starting `deckcraft <deck>` when it isn't running.
5. **`link.reveal {path, workbookId, anchor, sheet, range}`**: open the workbook if needed,
   activate the sheet, select the anchored range, scroll it into view, focus the window.
6. **`--goto <name | Sheet!A1>`** on the command line, doing what `link.reveal` does after the
   file opens.
7. Tests: payload round trip through `links::from_html` semantics (the attribute, base64), anchor
   follows inserted rows, `link.read` after an unsaved edit returns the new value.

GridCraft's Excel export already writes cached values (`<v>`), which is what DeckCraft reads from
the file; a workbook saved by Excel works the same way.
