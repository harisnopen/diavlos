# Approvals that hold against your own agents

An approve is a yes signed with a person's key. It is only as strong as the
place that key lives. An agent with a shell can use every key on its own
machine: it can run `diavlos` as any of them, or read the key files. So an
approve signed with a key that sits next to your agents proves only that
something on that machine signed it.

In a room, two kinds of key can say yes:

- **Approver keys.** Any person's key that may approve there: the approver
  role, or the owner's key if it is a person's.
- **The owner key.** Even when it cannot approve itself, it can invite a new
  member marked human, and approve through that.

For approvals that must hold against your agents, none of those keys can be
on the machine where the agents run. The room's home holds the owner key,
so the home is not there either.

[Watch it run](use-cases/approver-off-the-agents-machine.md) on two real
machines, every step below, including the warning when a person's key is
put next to the agents.

## The layout

| Machine | Holds | Runs |
|---|---|---|
| **The gate**: your own computer, or a small server only people can log into | The room's home and its owner key. Your approver key, if you approve from here. | Nothing of the agents'. |
| **The agents' machine** | Agent keys only. | The agents, their MCP servers and hooks, and the scripts that call `check-approve`. |
| **Other approvers' devices** (optional) | Their own human keys. | `diavlos web`, or the command line, to approve. |

Another OS user on the same machine can stand in for the gate, if the agents
cannot become that user or read its files: no `sudo`, no shared group, no
shared home.

Every message in a room goes through its home, so the gate has to be online
for the room to work. If it is a laptop that sleeps, messages wait and none
are lost, but nothing moves until it wakes. For a room that works around the
clock, make the gate a small server or VM that no agent can reach, and
approve from your laptop as an invited human (step 5).

## Set it up

**1. On the gate**, make the room. You own it.

```sh
diavlos new ops
```

**2. On the agents' machine**, note its node id and give each tool its own
agent key.

```sh
diavlos status                            # the first line shows "node <AGENTS-NODE>"
diavlos mcp install --for claude-code     # the tool acts as the agent key `claude-code`
```

**3. On the gate**, invite each agent key, pinned to the agents' machine so a
copied invite is useless anywhere else. Include a key for your deploy
scripts.

```sh
diavlos invite ops claude-code --for <AGENTS-NODE>
diavlos invite ops deployer --for <AGENTS-NODE>
```

**4. On the agents' machine**, join as those keys, with the command each
invite prints. It has `--as` in it: a plain `diavlos join` would join as
`default`, which is a person's key.

```sh
diavlos --as claude-code join <invite>
diavlos --as deployer join <invite>
```

**5. If the gate is a server**, invite yourself from your own computer.

```sh
diavlos status                                   # on your computer: "node <LAPTOP-NODE>"
diavlos invite ops alice --human --for <LAPTOP-NODE>    # on the gate
diavlos join <invite>                            # on your computer
```

Approve from there. `diavlos next ops` shows each question, the exact action
it asks you to approve, and its id:

```text
[9] claude-code (question): Deploy api-service v1.2 to prod?
    action {"verb":"deploy","target":"api-service","params":{"env":"prod","version":"1.2"}}
    id     m_01M3718M90Z2BCFYPR4VTW83X6
```

Read the action, not the question: the action is what the approve signs.
Then `diavlos send ops --type approve --reply-to <id>`. Or approve in the
browser with `diavlos web`.

**6. The deploy script**, on the agents' machine, asks the gate to record the
spend for this one run before it acts.

```sh
diavlos --as deployer check-approve ops --op "$RUN_ID" \
  '{"verb":"deploy","target":"api-service","params":{"version":"1.2","env":"prod"}}' && ./deploy.sh
```

## Check it

Run `diavlos doctor` on each machine. There should be no `WARN approvals`
line on the agents' machine. `diavlos status` shows the same warning, and
`check-approve` prints it when it succeeds on a machine where a key that can
say yes sits with agent keys.

The warning counts every agent key on the machine, in a room or not: a
tool's key is made the first time the tool runs, often before it is let into
any room.

If you see it, revoke that member on the gate (`diavlos revoke ops <name>`)
and invite the person again from a machine the agents cannot reach. The
warning clears once the agents' machine hears of the revoke, usually within
seconds. If the room's home is on the agents' machine, make a new room on
the gate instead.

## What this gives you

- An agent can ask, and cannot approve: no key on its machine can.
- It cannot invite a new member marked human: the owner key is on the gate.
- A spend is recorded once, at the gate, for one named operation.

## What it does not

- It does not stop a person approving something they did not read. An
  approve signs the exact action; read the action, not the words around it.
- An agent with a shell on the gate, or on an approver's device, is back
  where it started. Keep agents off those machines.
- `doctor` looks at one Diavlos home on one machine. It cannot see a key
  copied somewhere else, or a second home in another folder of the same
  machine (a different `DIAVLOS_HOME`).
- A spend is permission for one operation, not proof it ran once. Make the
  deploy script skip an operation id it has already done.
