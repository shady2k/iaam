# Architecture

How iaam is built is described by the crates' own docs, the design spec
(`.internal/specs/2026-08-22-investment-tracker-design.md`), the decision
records (`docs/decisions/`) and `scripts/check-architecture.sh`, which guards
the dependency direction. This document keeps only the target: the principles
every piece of work and every review is judged against.

The owner confirmed the list below whole on 2026-11-14 (iaam-391zq). Where the
code departs from a principle, the code is fixed; a principle changes only by
the owner's decision.

## Solution strategy

- **Functional core.** The core does no input or output: it receives a ready
  slice of data and computes. In practice: a new calculation is only a pure
  function in `iaam-core`, no I/O, no async, no Mutex, no binary floating
  point there; the shell gathers data and stores the result. Carried by:
  [design spec §3.1](../.internal/specs/2026-08-22-investment-tracker-design.md)
- **One dependency direction.** Transports are interchangeable because they
  know only the application layer. In practice: a new crate is only placed in
  the existing graph, `iaam-server` depends on `iaam-app` alone, adapters are
  assembled in `iaam-bootstrap`, no shared "utils" crate. Carried by:
  [design spec §3.2](../.internal/specs/2026-08-22-investment-tracker-design.md)
- **Money is counted only by the core, exactly.** Every figure in a response
  comes from the core, in exact decimals. In practice: a new amount in a
  response is only computed in `iaam-core`, no monetary arithmetic in the
  shell. Carried by:
  [design spec §3.4](../.internal/specs/2026-08-22-investment-tracker-design.md)
- **One outgoing HTTP gateway.** Outgoing HTTP lives only in `iaam-http`. In
  practice: a new external source is only reached through `Gateway::send`, no
  HTTP client of its own. Carried by:
  [design spec §3.1](../.internal/specs/2026-08-22-investment-tracker-design.md)
- **The journal of facts is immutable.** A recorded fact is never rewritten.
  In practice: a correction is only a new fact, no update of a recorded one.
  Carried by:
  [design spec §4.8](../.internal/specs/2026-08-22-investment-tracker-design.md)
- **A write route is an operation key.** A retried request writes nothing
  twice. In practice: a new write route is only idempotent by an operation key
  or says why it is not, no route that duplicates on retry. Carried by:
  [ADR 0025](decisions/0025-a-write-route-is-an-operation-key-or-says-why-it-is-not.md)
- **The agent's authority is bounded by what cannot be undone.** An agent may
  convey a document and may not interpret one. In practice: a new agent-facing
  action that cannot be undone is only taken on the owner's decision, no agent
  interpretation of a source document. Carried by:
  [ADR 0040](decisions/0040-the-agents-authority-is-bounded-by-what-cannot-be-undone.md)
- **The broker's data decides (P2).** What a source can tell is not asked of
  the owner. In practice: a new question to the owner is only one no source
  answers, no asking what the broker's report already says. Carried by:
  [vision](vision.md)
- **The owner's data never enters the repository (P1).** Nothing that
  identifies the owner or his money is committed. In practice: a new fixture is
  only invented data, no value taken from a real statement; `make privacy`
  guards the shape. Carried by: [vision](vision.md)
- **The application survives any interruption; no timer is the safety (P3).**
  «Приложение должно уметь переживать любую такую ситуацию. Решение таймером -
  плохое решение». In practice: a new write is only whole or visibly
  interrupted and completed on retry, no partial write and no timeout standing
  in for that. Carried by: [vision](vision.md)
- **A figure carries its source, date and status (P4).** A number without
  where it came from is not an answer. In practice: a new report figure is
  only published with its source, `as_of` and status, no bare amount. Carried
  by: [vision](vision.md)
