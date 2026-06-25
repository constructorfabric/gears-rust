# Feature: Kitsoki web UI

The kitsoki web UI is a single-page app served by `kitsoki web`. It lets an
operator browse the catalogue of available stories, open a session against one,
observe the recorded trace of a run, and drive a session forward by submitting
intents — watching the state badge and chat transcript update each turn.

Key surfaces a demo of this feature should exercise:

- **Home / catalogue** — a grid of story cards (title + description), with
  controls to start a new session or rescan the stories directory.
- **Session / observe** — a breadcrumb, the recorded view of the current room,
  and a reload control.
- **Drive / interactive** — a chat transcript, a composer (intent picker + text
  input + send), and a prominent **state badge** that is the hard signal a turn
  landed.

This file is free-form prose. The reviewer reads it for context; the concrete
pass/fail checks come from the scenarios file.
