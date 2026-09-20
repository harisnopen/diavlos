# Security policy

## Reporting a problem

Please report security problems privately, not in a public issue.

Use GitHub's private vulnerability reporting for this repository:
https://github.com/harisnopen/diavlos/security/advisories/new

We aim to acknowledge a report within 3 working days and to say what we plan
to do within 14 days.

## What we protect

Three things: the wire, the door, and the agent's head. Code locks the first
two; the third needs a human in the loop. The full write-up is in
[docs/THREAT-MODEL.md](docs/THREAT-MODEL.md).

## Known limits in v0.1

- Keys and the inbox sit on disk in plain text (0600 on Unix). OS keychain
  and an encrypted inbox come in v0.2.
- The relay cannot read messages but can see who talks to whom, when, and
  how much.
- A stolen, unlocked laptop is you. Same as SSH keys. Rotate the room and
  re-invite.
- On Windows the local pipe is not yet restricted to your user account.
