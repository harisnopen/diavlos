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
