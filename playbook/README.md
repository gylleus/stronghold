# Fleet playbook

The rules Karl's agent fleet runs by. Myrstack owns and serves these files; edit them here and
commit, so every rule change has a diff and a history.

- `AGENT-FLEET-PLAYBOOK.md`: coordinators (and the fallback attendant).
- `_AGENT_PROTOCOL.md`: every dispatched worker.
- `ATTENDANT-BRIEF-TEMPLATE.md`, `KICKOFF-TEMPLATE.md`: templates filled in per Run / track.

`~/Documents/agents/` holds symlinks to these files, so older kickoff specs and memory files
that point there keep working.
