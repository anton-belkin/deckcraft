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
(`deckcraft --agent-stdio`), or through AItoCraft: the user picks **Claude Cowork** in the Context
group of DeckCraft's Agent tab and follows **Get ready** (Install AItoCraft, Ask Cowork to
collaborate), then opens Claude. If a tool says "DeckCraft isn't open", ask the
user to open DeckCraft, then call it again.

## Through AItoCraft (several decks, several conversations)

When the tools come from the `aitocraft` server, the person says which deck this conversation is
for ("we're working on board.pptx"): call `list_documents`, then `attach` with that deck's id and a
label (the client and project). Keep the document id and your listener id, and pass both on every
call (`document`, `listener`): another conversation may be working on another deck at the same
time. Listen with `wait_for_questions {document, listener}` in a loop; in a Claude Code session
started with the channel, questions arrive as messages by themselves.

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

A question with kind `ping` is a connection check: answer it at once with "here", don't
investigate.

When DeckCraft sends questions to your Claude Code session (`notifications/claude/channel` from
the `deckcraft-channel` server), each one arrives as a message with its `question_id`: handle it
like any other question.

## Topics

Every question belongs to a **topic**: one question or theme about the deck, with places (shapes,
words, slides; numbered 3, or 3.1, 3.2 when it has several), a status (Open, Revise, Agreed, For
information) with a line of text, notes, and the discussion. The deck shows each place as a small
numbered circle in the status colour. A question carries its topic's number and status: answer it
in that context (`get_topic` gives the notes and the discussion so far; `list_topics` all of them).

- **Link inside the text.** Write `[slide 4](slide:4)`, `[the chart](shape:4/12)`,
  `[3.2](place:3.2)`, `[topic 3](topic:3)`, `[the source](note:3/n2)` in answers, notes and status
  text; a plain "slide 4" is linked by itself. Links follow the deck when slides move.
- **Add places that a change here affects** (`add_places` with a `reason`, e.g. "the 6% is computed
  from this table"), and `widen_place` when the answer is about more than what was selected.
- **Leave notes that will still matter after the chat** (`add_note` with the `topic`: sources with
  `url` and `summary`, assumptions, decisions); the discussion is dropped when the deck is sent.
- **Never mark Agreed: propose it** (`propose_agreed` with your reasons). You may `set_status` to
  Open, Revise or For information, with a text that says why.

A question the user takes back (⌘Z) is withdrawn: `answer_question` then says "this question was
withdrawn by the user; drop it". Drop it.

## For each question

⟦n⟧ in a question is what the user pointed at while speaking (a shape or a slide), see `refs`.

1. `claim_question` at once: the place's marker turns blue, so the user sees you are on it.
2. Investigate: `get_context` (once per session), `read_slides` for the slide (`where.slide`) and
   the slides around it, `list_links` with `check: true` when the question is about a number in a
   linked table (it says which workbook cells it comes from and whether they changed).
3. `answer_question`: short paragraphs, the answer first, with the links inside the text (above):
   the slides and shapes you rely on (`shape:3/12` = slide 3, shape id 12 from `read_slides`), your
   notes and sources. If the premise is wrong, say so and show where.
4. If the audience is likely to ask the same, add it as a prepared answer (`add_note`, kind `faq`,
   `title` = the question). If a decision was made, `log_decision`. Then the topic's status: Revise
   when something must change, `propose_agreed` when it is settled.

## Prepare a deck (when asked, or when idle)

- For every slide, 1–3 prepared answers (`add_note` kind `faq`, `slide`: n) to the questions a
  sharp audience asks: where a number comes from, what changed since last time, what is not
  included, what happens next. The presenter sees them in Presenter View under the speaker notes.
- `list_links` with `check: true`: a `stale` link means the workbook changed since the paste
  (DeckCraft raises the topics on it to Revise). Say what changed in a `check` note on the topic;
  don't update it yourself unless asked.
- `add_note` kinds `source`, `assumption`, `explanation`, `check` on the shapes they are about.

## Changing the deck

Only through `run_command`, with a `reason` and a `source`; it is recorded as yours in the deck's
history. Reading commands, files and the clipboard are refused (use `read_slides`). Commands act on
the selection unless they take `id`/`ids`.

**In a topic, propose; don't edit.** `run_command {…, topic}` doesn't change the deck: it runs on
the topic's draft and is recorded in its proposal; the person applies it in the document. Don't ask
for confirmation in the chat. When there's a real choice, offer options (`option: "B"`, then
`describe_option {topic, option, title, why}`); take one back with `discard_proposal`. Check a draft
with `read_slides {view: "proposal", topic, option}`. Shapes a pending proposal touches carry
`pending`, questions carry `pendingNearby`: build on them, don't contradict them. A rejected
proposal's comment comes back as a question in the topic; an out-of-date one (the deck changed
under it) needs redoing.

## Privacy — non-negotiable

Text the user marked as client data shows as pseudonyms like `[client-3]`, in the marked shape and
wherever the same names and figures appear elsewhere in the deck. The same text always gets the
same pseudonym. So:

- never present a pseudonym as a real name or number and never guess what it stands for; refer to
  "the client" or the pseudonym;
- never try to find the real text (no reading the file from disk, no control port);
- you can't change shapes holding client data, or delete slides that hold them; that's intended.
