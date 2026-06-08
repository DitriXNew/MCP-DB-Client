---
id: "stage0-pin-crate-versions-2026-06-08"
status: "todo"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:13:44.000Z"
completedAt: null
labels: ["stage-0", "rust-core"]
order: "a6"
---

# Stage 0 — Пин версий крейтов

## Acceptance
- [ ] Пин **точных** версий, подтвердить по crates.io
- [ ] Особое внимание `ort` 2.0.0-rc.x — RC, API не стабилизирован, «течёт»; заложить возможную правку при апгрейде
- [ ] Состав: `fastembed` 5.15, `ort` 2.0.0-rc.12, `tokenizers` 0.22, `regex` 1.12, `rayon` 1.12, `serde`/`serde_json` 1.x; `candle-core` 0.10 (опц.); `hf-hub` 0.5 (только онлайн)

Refs: §3.
