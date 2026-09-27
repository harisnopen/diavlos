# Use cases, filmed

Each page is a real run on real machines, filmed as it happened, with the
video, a transcript and the scripts that ran it.

| page | what you see |
|---|---|
| [Two cloud desktops, one AI agent, one human](two-cloud-desktops.md) | A real Claude agent takes tasks, refuses a prompt injection, and waits for a human to approve a deploy. |
| [The approver off the agents' machine](approver-off-the-agents-machine.md) | Approvals that hold against your agents: the approve is spent once, and `doctor` warns when a person's key sits next to the agents. |
| [A file there and back](files-between-two-desktops.md) | A file goes to an agent and comes back changed; the fingerprint is checked; a script arrives as `.unsafe`. |
| [Wake an agent that is not running](wake-an-agent.md) | A task starts the agent through a wake rule; three tasks at once wake it once. |
| [The room's home goes away and comes back](home-offline-and-back.md) | Messages and a file wait in the outbox, then all go in, in order. |
| [Two agents, one task](two-agents-one-task.md) | Two agents claim one task at the same moment; one wins, the other is told no. |
| [Stop a runaway agent](stop-a-runaway-agent.md) | Pause the room, mute one agent, revoke its key. |
| [What does not get sent](what-does-not-get-sent.md) | Secrets are refused on the sender's machine; a "safe" room refuses programs. |
| [Chains of hand-offs that stop](chains-that-stop.md) | Work handed back in a circle, or passed on too many times, is refused. |
| [An audit you can check](an-audit-you-can-check.md) | The room's signed log, checked by an auditor; one changed word is caught. |

Two E2B sandboxes in one room are filmed too: see the main
[README](../../README.md) and [the example](../../examples/e2b-sandboxes/).
