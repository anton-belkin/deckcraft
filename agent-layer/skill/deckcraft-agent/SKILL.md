---
name: deckcraft-agent
description: Work as the user's agent inside an open DeckCraft presentation through its agent layer — answer questions the user asks on slides, shapes and numbers while they keep working, prepare answers to the audience's likely questions per slide, check linked spreadsheet numbers, and change the deck with a recorded reason. Use when the user has DeckCraft open and asks you to watch for their questions, review or prepare a deck, or get it ready to present.
---

# Working in a DeckCraft deck as the user's agent

The user presents this deck or signs it off. Your job is to make that fast and safe: answer what
they ask where they ask it, prepare them for what the audience will ask, and flag numbers that no
longer match their source. They keep working in the deck; you work in the background.

## Connect

DeckCraft runs an MCP server itself: `http://127.0.0.1:7984/mcp`. If its tools (`get_context`,
`read_slides`, …) aren't available, ask the user to run
`claude mcp add --transport http deckcraft http://127.0.0.1:7984/mcp` (or check DeckCraft is open:
`curl -s http://127.0.0.1:7984/health` prints `ok`).

In Claude Desktop or Cowork the same tools come from the local `deckcraft` server
(`deckcraft --agent-stdio`); the user adds it with **Agent ▸ Connect Cowork** on DeckCraft's
ribbon, then quits Claude (⌘Q) and reopens it. If a tool says "DeckCraft isn't open", ask the
user to open DeckCraft, then call it again.

## Watch for questions

Start a background monitor on the event stream; each line is a new question:

```bash
curl -sN http://127.0.0.1:7984/events
```

(With the Monitor tool: run that command as the monitored process.) Without a monitor (Claude
Desktop, Cowork: there is no event stream over stdio), call `wait_for_questions` in a loop: each
call returns as soon as a question is pending, or empty after up to 50 s; answer what it returned,
then call it again. Stop when the user says so. On startup also call `list_questions` with status
`open`: questions asked before you connected are waiting too.

## For each question

1. `claim_question` at once: the shape shows the user you are on it.
2. Investigate: `get_context` (once per session), `read_slides` for the slide (`where.slide`) and
   the slides around it, `list_links` with `check: true` when the question is about a number in a
   linked table (it says which workbook cells it comes from and whether they changed).
3. `answer_question`: short paragraphs, the answer first. Link the slides and shapes you rely on
   (`slide:3`, `shape:3/12` = slide 3, shape id 12 from `read_slides`), your notes (`note:n3`) and
   sources (https URLs with a one-line `summary`). If the premise is wrong, say so and show where.
4. If the audience is likely to ask the same, add it as a prepared answer (`add_note`, kind `faq`,
   `title` = the question). If a decision was made, `log_decision`.

## Prepare a deck (when asked, or when idle)

- For every slide, 1–3 prepared answers (`add_note` kind `faq`, `slide`: n) to the questions a
  sharp audience asks: where a number comes from, what changed since last time, what is not
  included, what happens next. The presenter sees them in Presenter View under the speaker notes.
- `list_links` with `check: true`: a `stale` link means the workbook changed since the paste. Tell
  the user (`log_decision` or a `check` note on the shape); don't update it yourself unless asked.
- `add_note` kinds `source`, `assumption`, `explanation`, `check` on the shapes they are about.

## Changing the deck

Only through `run_command`, with a `reason` and a `source`; it is recorded as yours in the deck's
history. Reading commands, files and the clipboard are refused (use `read_slides`). Commands act on
the selection unless they take `id`/`ids`.

## Privacy — non-negotiable

Text the user marked as client data shows as pseudonyms like `[client-3]`, in the marked shape and
wherever the same names and figures appear elsewhere in the deck. The same text always gets the
same pseudonym. So:

- never present a pseudonym as a real name or number and never guess what it stands for; refer to
  "the client" or the pseudonym;
- never try to find the real text (no reading the file from disk, no control port);
- you can't change shapes holding client data, or delete slides that hold them; that's intended.
