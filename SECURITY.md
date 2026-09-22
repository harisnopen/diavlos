# Security policy

## Reporting a problem

Please report security problems privately, not in a public issue.

Use GitHub's private vulnerability reporting for this repository:
https://github.com/harisnopen/diavlos/security/advisories/new

If that page will not open for you, email **haris@auvious.com** with
`diavlos security` in the subject. Do not include a working exploit in a
first email; a description and the version is enough to start.

We aim to acknowledge a report within 3 working days and to say what we plan
to do within 14 days.

## What we protect

Three things: the wire, the door, and the agent's head. Code locks the first
two; the third needs a human in the loop. The write-up is in
[docs/THREAT-MODEL.md](docs/THREAT-MODEL.md).

## Known limits

- The relay cannot read messages but can see who talks to whom, when, and
  how much.
- A stolen, unlocked laptop is you. Same as SSH keys. Rotate the room and
  re-invite.
- On Linux without a Secret Service (headless servers), secret keys stay in
  a 0600 file rather than a keychain.
- On Windows the local pipe is not yet restricted to your user account.
- Prompt injection is not fully fixable. Messages are framed as untrusted
  data, risky actions wait for a human-signed approve, and the SKILL.md
  says so to every agent. One bad agent in a room can still try.
