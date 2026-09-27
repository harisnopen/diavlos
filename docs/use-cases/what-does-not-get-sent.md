# Use case: what does not get sent

A filmed run on two real machines. Two rented cloud desktops (Orgo.ai,
Ubuntu 24.04) talk over the public internet. An agent on desktop B tries
to send a `.env` file with a cloud key in it, then a password in a
message. Both are refused on B itself, so they never leave the machine.
Then the owner sets the room to take only safe files: a picture goes in,
a program named `.png` and a shell script do not.

Recorded 2026-09-27. Room `ops`. 3 signed messages. Diavlos 2.1.0,
installed on each desktop with [`install.sh`](../../install.sh). The key in
the `.env` file is AWS's published example key, not a real one.

![Both desktops, side by side](media/notsent-two-desktops.gif)

Video: [media/notsent-two-desktops.mp4](media/notsent-two-desktops.mp4)
(25 s, both desktops side by side, 74 s of real time; the clock in the
top right is real time). Transcript of both windows:
[media/notsent-transcript.txt](media/notsent-transcript.txt).

## Who was where

| machine | key | what it does |
|---|---|---|
| desktop A | `desk-a`, a person, the owner | the room's home; sets the room's rules |
| desktop B | `bot`, an agent | tries to send things it should not |

## What happened, in order

1. **A secret in a file.**

   ```
   $ cat .env
   DB_HOST=db.internal
   AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
   $ diavlos --as bot send ops 'here is the env' --file .env
   error: denied: refused: env looks like it holds a AWS access key, so it was not sent. (secret_scan = false in config turns the scan off)
   ```

2. **A secret in a message.**

   ```
   $ diavlos --as bot send ops 'the db password = Tr0ub4dor-and-3-horses-9'
   error: denied: refused: that looks like a assignment that looks like a secret, so it was not sent. (...)
   ```

   Both are refused by B's own helper before anything is sent. B's
   outbox is empty, and on A, `read` shows nothing from bot.

3. **The owner allows only safe files.** On A, the room's home:

   ```
   $ echo 'files = "safe"' >> ~/.diavlos/rooms/ops/policy.toml
   ```

   It takes effect at once; nothing restarts.

4. **A picture goes in.** `--file chart.png`, a real PNG: sent.

5. **A program does not, whatever its name.** `/bin/true` copied to
   `tool.png`, and a shell script `fix.sh`:

   ```
   error: denied: room ops takes only plain text, pictures, PDF and ZIP (files = "safe"); that file looks like program
   ```

   The home looked at the bytes, not the name. On A, only the picture
   came in.

## What this shows

- **Common secrets stop at the sender.** Text, data, actions and text
  files are scanned on the sender's own machine, so a key pasted by
  mistake never reaches the room.
- **The owner decides what kinds of files a room takes.** `safe` means
  text, pictures, PDF and ZIP, checked by content.

## What it does not show

- A secret someone means to get out. The scan catches accidents; base64
  or a split string gets past any pattern. See
  [the threat model](../THREAT-MODEL.md).
- `files = "off"`, which refuses every file.

## Found while filming

Two messages read badly. Both are only wording:

- "holds a AWS access key", "looks like a assignment that looks like a
  secret": the article is always "a".
- "that file looks like program" is missing "a".

## How it was filmed

The steps are in [`play-notsent.py`](media/desktop-film/play-notsent.py).

The scripts are in [media/desktop-film](media/desktop-film).
[`take.py`](media/desktop-film/take.py) runs one film. It sends each step
to its desktop through the Orgo API, one shell command at a time (the
module that holds the account details is not included). Before filming,
[`film.py`](media/desktop-film/film.py) gives each desktop a clean home,
installs diavlos with `install.sh`, makes the room and joins the agents,
off camera. On camera, [`do.sh`](media/desktop-film/do.sh) types each
command into its window, runs it and shows the output. The home folder
is shown as `~`, and invite tokens and node ids are cut short.

[`rec.sh`](media/desktop-film/rec.sh) takes a screenshot of each desktop
every 1.5 s, on the same beat on both. The two desktop clocks were
seconds apart, so each got its measured offset.
[`stitch.py`](media/desktop-film/stitch.py) puts each pair side by side
at 2 frames per second.
