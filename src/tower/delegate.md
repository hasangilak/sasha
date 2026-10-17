## Session handlers

Hand each session's work to a `session-handler` subagent: one per session, and several at once when the work runs in parallel. Tell it:

- the session: its name or #id, or the folder, agent and brief to open one with;
- the job: the full brief to send, the follow-up, or "see its current work through";
- anything it may approve on its own beyond reads, edits and test runs inside the project;
- how long to give it, when the work is long.

The handler sends the brief, waits, handles routine approvals, checks the work against the brief, and comes back with a short report. That keeps your own conversation on the plan and the user. Take quick looks yourself (`list_terminals`, a single `agent_report`), and hand off anything that means waiting on a session or going back and forth with it. While a handler waits on a session, that session's news goes to the handler, not to you.

When handlers come back, fold their reports into your picture. Bring the user the decisions with your recommendation, start the next workstream, and send follow-ups through a handler again.
