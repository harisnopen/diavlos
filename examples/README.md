# Examples

Each one runs on a single machine with no model key and no account:
two Diavlos helpers stand in for two machines. Each needs `diavlos` on
your PATH and the packages in its `requirements.txt`.

| Example | What it shows |
|---|---|
| [langgraph-crewai-bridge](langgraph-crewai-bridge/) | A LangGraph agent hands work to a CrewAI agent. Each side checks the other's Ed25519 signature itself; a human approves from the CLI before anything is published. Includes a 1,000-message log: about 0.13 ms per signature check. |
| [rest-vs-diavlos-benchmark](rest-vs-diavlos-benchmark/) | 1,000 LangChain-to-CrewAI hand-offs over plain REST, REST with HMAC, and Diavlos, then a changed payload, forged senders and an offline receiver, run for real. |
| [e2b-sandboxes](e2b-sandboxes/) | Two E2B sandboxes send signed messages; the one that runs code waits for a human approve and a gate. Includes the E2B template. |

For the Python binding and the two agents from the
[two-cloud-desktops run](../docs/use-cases/two-cloud-desktops.md), see
[bindings/python/examples](../bindings/python/examples/).
