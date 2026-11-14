# iaam — vision

## Direction

iaam is the owner's personal accounting system, operated by agents. The owner
asks; an agent obtains finished figures from iaam's REST API and does not
rebuild accounting logic itself. The API owns grouping, arithmetic, duplicate
elimination and confidence; the client owns presentation (owner, 2026-11-14:
«должны быть готовые методы в rest api»).

## Audience

The owner, through agents.

## Principles

- **P1. The owner's data never enters the repository.** See `CLAUDE.md`.
- **P2. The broker's data decides.** What a source can tell is not asked of the
  owner.
- **P3. The application survives any interruption; no timer is the safety.**
  (owner, 2026-11-14: «Приложение должно уметь переживать любую такую
  ситуацию. Решение таймером - плохое решение»)
- **P4. A figure carries its source, date and status.**

## Journeys

- **J1.** The owner sees his current assets in one answer.
- **J2.** The owner sees how much he put into and took out of investments, per
  month and year.
- **J3.** The owner's sources (brokers, banks) arrive in iaam without hand work.

## Exclusions

- iaam sends nothing to a broker; it only reads.
- No trading.

## Roadmap

1. m-ready-answers — "Answers are ready, and iaam does not break" (current;
   charter: `docs/charters/m-ready-answers.md`).
2. "Bank statements arrive in iaam without hand work".

Status of any item comes from the tracker, never from this document.
