# Documentation

This directory holds documentation that does not belong in rustdoc: the shape
of the system, the reasoning behind it, and the constraints a reader needs
before touching the code. API reference lives in doc comments next to the code,
where it cannot drift.

## Layout

```text
docs/
├── README.md      # this index
├── research/      # dated research reports and their synthesis
├── specs/         # behavior and architecture specifications
├── plans/         # implementation plans derived from approved specs
└── adr/           # architecture decision records, numbered and immutable
```

- **[`research/`](research/README.md)**: the research behind the design, kept
  as dated reports, plus a synthesis with the problem in numbers, the guiding
  principles and a catalogue of ideas by pillar.
- **[`specs/`](specs/README.md)** — one file per feature, module, or subsystem,
  describing its behavior, public surface, invariants, and acceptance criteria.
- **[`plans/`](plans/README.md)** — implementation-ordered, test-first steps for
  delivering an approved specification. Plans name exact files and verification
  commands, and are updated as the work progresses.
- **`adr/`** — a dated record per significant decision. Use
  [`adr/0001-record-architecture-decisions.md`](adr/0001-record-architecture-decisions.md)
  as the template. An accepted ADR is not edited; it is superseded by a later
  one.

Complex modules also carry a module-level `README.md` inside `src/<module>/`
covering their design, public surface, and important constraints.

Start with the target architecture in
[`specs/architecture.md`](specs/architecture.md) and the phased delivery plan
in [`plans/roadmap.md`](plans/roadmap.md). The research that motivates both is
summarized in [`research/README.md`](research/README.md).

## Conventions

- Keep every Markdown file at 500 lines or fewer. When a topic outgrows that,
  split it into focused files and link them from the nearest `README.md`.
- Update documentation in the same commit as the behavior it describes.
- Prefer a concrete example over an abstract description.
- Link between documents rather than duplicating content; one fact lives in one
  place.
- Write a specification before a plan: the spec defines the outcome and
  constraints, while the plan defines the implementation sequence.
