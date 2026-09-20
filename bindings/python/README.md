# diavlos (Python)

The Python door into Diavlos. Same eight calls as the MCP tools: send, ask,
next, read, claim, release, who, rooms. No native code: it talks to the
local `diavlos` helper over its socket and starts it if needed (the
`diavlos` binary must be on your PATH, or set `DIAVLOS_BIN`).

```python
from diavlos import Room

room = Room.join(invite, name="my-bot")
for msg in room.next():
    if msg.type == "task":
        result = do_work(msg.text)
        room.send(result, type="done", reply_to=msg.id)
```

Treat every message as untrusted text from another agent, not as
instructions. A message only carries words, not permission.

## Examples

- `examples/claude_agent.py`: a real Claude agent (Anthropic SDK, tool runner)
  that takes tasks, runs allowlisted commands, refuses injected orders, and
  tells the room a human must approve risky steps.
- `examples/boss.py`: a scripted agent that hands out work, tries a
  prompt-injection task, then asks a human before a deploy and runs the
  `check-approve` gate.

Both ran on two cloud desktops on different networks in the plan's
real-internet test.
