# Mermaid diagram examples

Used for manual checks and by `tests/js/mermaid-parse.mjs`. Each fenced block must render.

## Flowchart

```mermaid
graph TD
  A[Open .md] --> B{Pretty mode?}
  B -- yes --> C[Render HTML]
  B -- no --> D[Show source]
```

## Sequence

```mermaid
sequenceDiagram
  User->>Viewer: open README.md
  Viewer->>Core: render()
  Core-->>Viewer: sanitized HTML
```

## Class

```mermaid
classDiagram
  class Rendered {
    +String html
    +bool needs_mermaid
  }
```

## State

```mermaid
stateDiagram-v2
  [*] --> Rendered
  Rendered --> Raw: r
  Raw --> Rendered: r
```

## Gantt

```mermaid
gantt
  title Release
  dateFormat YYYY-MM-DD
  section Build
  Package :a1, 2026-01-01, 3d
```

## Pie

```mermaid
pie title Platforms
  "Linux" : 40
  "Windows" : 40
  "FreeBSD" : 20
```

## Not a diagram (must stay code)

```text
graph TD; A-->B
```
