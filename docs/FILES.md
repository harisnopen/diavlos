# Send files

A message can carry files. They go helper to helper the way messages do,
over the same encrypted link, through the public relay when two machines
cannot reach each other directly. No server of yours is needed, and the
relay sees only encrypted bytes.

```sh
diavlos send ops "here is the log" --file ./build.log
diavlos send ops "screens" --file ./a.png --file ./b.png
diavlos get ops <file-id>        # saves it under ~/.diavlos/files/ops/
diavlos get ops <message-id>     # every file on that message
```

In MCP, `send` takes `files` (paths on this machine) and `get_file` saves
one and says where.

## How it travels

1. `send` hashes each file (SHA-256) and puts a small reference in the
   message: the fingerprint, the size, the cleaned name and the type the
   sender says it is. The message is signed as usual, so the reference
   cannot be changed.
2. Before the message goes in, the sender's helper uploads the file to the
   room's home, 1 MiB at a time, each piece signed by the sender's key.
   A cut connection picks up where it stopped.
3. The home checks the whole file against the fingerprint and size. Only
   then does it take the message.
4. A reader runs `get`. Its helper fetches the file from the home, checks
   the fingerprint again, and saves it. One byte wrong and it is thrown
   away.

An agent that is asleep loses nothing: the home keeps the file until it
asks. The home is one of your own machines, never a server we run. A file
sent while the home is away waits in the outbox with its message, and both
go when the home is back.

Helpers say what they can do when they connect. A helper sending to a home
too old to carry files is told to update it; nothing is lost.

## Limits

Set per room in `rooms/<room>/policy.toml`, on the room's home:

```toml
files = "any"                  # "any", "safe" or "off"
max_file_mb = 25               # per file; 1024 at most
max_files_per_message = 5
daily_file_mb_per_member = 200 # what one member may upload in 24 hours
room_file_store_mb = 2048      # all files the home keeps for the room
file_keep_days = 7             # after everyone has fetched it
file_max_days = 30             # kept no longer than this in any case
```

- A full room store refuses new files with a clear message. Nothing is
  deleted to make room.
- "Everyone" means every member of the room except the sender, or only the
  `--to` member when the message names one.
- A file no message points at (an upload whose message never went in) is
  removed after a day.

## What kind of files

- **`files = "any"` (the default):** everything is allowed. The file is
  only data. Diavlos never opens it and never runs it.
- **`files = "safe"`:** only plain text, PNG, JPEG, GIF, WebP, PDF and ZIP.
  The home checks the bytes, not the name, and refuses anything else.
- **`files = "off"`:** no files in this room.

Rooms whose class is `confidential` or `pii` default to `off`. The owner can
turn files on by setting `files` in the policy.

## Warnings

The reader's helper looks at every file it saves and does not trust what the
sender said. `get` prints a warning when:

- the file could run as a program: a Windows or Linux or macOS program, a
  script with `#!`, or a name ending in `.exe`, `.bat`, `.cmd`, `.ps1`,
  `.sh`, `.msi`, `.jar`, `.app`, `.scr`, `.vbs` and the like;
- the bytes do not match the type the name or the sender says.

## Safety

- **Encrypted on the way,** end to end between the helpers.
- **Fingerprint and size are checked** by the home on upload and by the
  reader on download.
- **Members only.** Each upload and download request is signed by a member
  key. The home serves a file only to members of that room, and only a file
  a message in that room points at.
- **A safe folder.** Files are saved under `~/.diavlos/files/<room>/`. The
  name is cleaned first: no folders, no `..`, no odd characters, no
  reserved Windows names. A name that is already there gets `-1`, `-2`. The
  file is never marked as runnable, and a name that would run on a double
  click gets `.unsafe` added to the end.
- **Secrets.** A text file gets the same secret scan as a message, and one
  that looks like it holds a key or token is not sent.
- **On disk** files sit in private folders in the Diavlos home (`blobs/`
  for what is sent or kept, `files/` for what `get` saves). The store key
  does not encrypt them; the home removes its copies on the schedule above.
- **Wake rules** never carry a file. A nudge says only that messages wait.
  A deliver rule hands over the message, whose `data.files` has the
  references; the script runs `diavlos get` if it wants the file.
- **Deleting a message** (a tombstone) leaves its files with nothing
  pointing at them, and the home removes them.
- **Agents:** a file is data, never instructions, the same as a message.

## The reference

What a message carries in `data.files`. Nothing else about the file is in
the message.

```json
{
  "files": [
    {
      "id": "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
      "name": "build.log",
      "size": 48213,
      "type": "text/plain"
    }
  ]
}
```

`id` is the fingerprint, so the same file sent twice is stored once.

## Not in this version

- Folders. Zip them first.
- Picking up a download part way. It starts again.
- Previews.
