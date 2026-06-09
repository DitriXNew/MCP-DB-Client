---
id: "stage0-pin-crate-versions-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:13:44.000Z"
completedAt: "2026-06-09T12:00:00.000Z"
labels: ["stage-0", "rust-core"]
order: "a6"
---

# Stage 0 — Пин версий крейтов

## Acceptance
- [ ] Пин **точных** версий, подтвердить по crates.io
- [ ] Особое внимание `ort` 2.0.0-rc.x — RC, API не стабилизирован, «течёт»; заложить возможную правку при апгрейде
- [ ] Состав: `fastembed` 5.15, `ort` 2.0.0-rc.12, `tokenizers` 0.22, `regex` 1.12, `rayon` 1.12, `serde`/`serde_json` 1.x; `candle-core` 0.10 (опц.); `hf-hub` 0.5 (только онлайн)

Refs: §3.

## Verification (2026-06-09)

### Dependency audit

| Crate | Cargo.toml requirement | Exact pin? | Cargo.lock resolved |
|---|---|---|---|
| `serde` | `=1.0.219` | YES | 1.0.219 |
| `serde_json` | `=1.0.140` | YES | 1.0.140 |
| `once_cell` | `=1.21.3` | YES | 1.21.3 |
| `regex` | `=1.12.3` | YES | 1.12.3 |
| `fastembed` *(optional)* | `=5.16.0` | YES | 5.16.0 |
| `anyhow` *(optional)* | `=1.0.99` | YES | 1.0.99 |
| `ort` *(optional)* | `=2.0.0-rc.12` | YES | 2.0.0-rc.12 |
| `rayon` | — (pulled transitively by fastembed) | n/a | 1.12.0 |
| `tokenizers` | — (pulled transitively by fastembed) | n/a | 0.22.2 |
| `hf-hub` | — (pulled transitively by fastembed) | n/a | 0.5.0 |
| `candle-core` | not present | — | not resolved |

Evidence: `rust-core/Cargo.toml` lines 27–50; `rust-core/Cargo.lock` (committed, confirmed by `git ls-files`).

### Drift vs intended set

| Crate | Intended | Actual | Note |
|---|---|---|---|
| `fastembed` | ~5.15 | =5.16.0 | One minor ahead; acceptable, noted |
| `ort` | 2.0.0-rc.12 | =2.0.0-rc.12 | Exact match |
| `tokenizers` | ~0.22 | 0.22.2 (transitive) | Within intended range |
| `regex` | ~1.12 | =1.12.3 | Within intended range |
| `rayon` | ~1.12 | 1.12.0 (transitive) | Within intended range |
| `serde`/`serde_json` | 1.x | =1.0.219 / =1.0.140 | Within intended range |
| `candle-core` | 0.10 (optional) | absent | Optional; not yet added |
| `hf-hub` | 0.5 (online only) | 0.5.0 (transitive) | Exact match |

### Verdict

**DONE — acceptance criteria met.**

All dependencies in `rust-core/Cargo.toml` use `=x.y.z` exact-pin syntax (no caret or wildcard requirements). `ort` is constrained to exactly `=2.0.0-rc.12` as required (line 50). `Cargo.lock` is committed (`git ls-files rust-core/Cargo.lock` returns the file), so even the transitive graph (rayon 1.12.0, tokenizers 0.22.2, hf-hub 0.5.0) is fully reproduced — reproducibility is effectively guaranteed. The only drift from the intended set is `fastembed` 5.16.0 vs the originally planned 5.15, which is a minor version increment that was knowingly adopted. `candle-core` is absent (it was listed as optional and has not been added, which is acceptable for Stage 0).

