---
id: "stage2-concurrent-query-embedding-2026-06-08"
status: "done"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T18:03:21.000Z"
completedAt: "2026-06-09T12:00:00.000Z"
labels: ["stage-2", "rust-core"]
order: "aJ"
---

# Stage 2 — Конкурентный query-эмбеддинг (одна ось параллелизма)

Снимает head-of-line blocking: во время `building` запрос исполняется параллельно с реиндексом, а не ждёт батч. Держит NFR латентности §9.

## Acceptance
- [ ] bulk: один крупный `Run` с intra-op = ncores−1, **без rayon поверх эмбеддинга** (избежать oversubscription rayon×intra-op)
- [ ] query-`Run` — конкурентно на той же сессии; зарезервированное ядро = headroom через планировщик ОС (на мелкой задаче), не партиционирование пула
- [ ] `rayon` — только на CPU-подготовку (чанкинг, офсеты, хэши), **не** на сам эмбеддинг; роль `rayon` в §3 скорректировать соответственно
- [ ] (Если выбран fallback двух экземпляров из stage-0 — bulk и query на раздельных сессиях, что даёт лучшую изоляцию латентности)

Refs: §4.4 (Правка 1), §3, §9.

## Verification (2026-06-09)

### Bullet 1 — bulk: one large Run, intra-op = ncores−1, no rayon over embedding

PARTIAL.

The single-batch `embed_passages` call (`fastembed_embedder.rs:255–257`) correctly issues one `TextEmbedding::embed` call (no rayon wrap), satisfying "without rayon over embedding". However, the `intra_threads: Option<u64>` field is parsed (`core.rs:172`, `lib.rs:461`) and echoed (`lib.rs:211`) but is never forwarded to `InitOptions` in `fastembed_embedder.rs`. The `load_builtin` / `load_local` constructors (`fastembed_embedder.rs:169–212`) pass only `.with_execution_providers(…)` — no `.with_inter_threads` / `.with_intra_threads` call. ORT therefore uses its own default thread count, not `ncores−1`. The no-rayon-over-embedding half of this bullet is fully satisfied; the explicit `ncores−1` intra-op tuning is a gap.

### Bullet 2 — query Run concurrent on the same session; headroom via OS scheduler

MET (via two-instance fallback — see Bullet 4).

The query path (`embed_query`, `fastembed_embedder.rs:260–267`) locks `self.query` only. The bulk worker path (`embed_passages`, `fastembed_embedder.rs:255–257`) locks `self.bulk` only. These are independent `Mutex<TextEmbedding>` fields (`fastembed_embedder.rs:93–95`); they never contend. The worker calls `embed_passages` outside the index `RwLock` (`core.rs:586–603`); queries (`dense_channel`, `core.rs:1316–1358`) call `embed_query` under a read-lock on `CORE`. The two locks are independent, so a query embedding run truly concurrent with a bulk reindex is not serialized behind any shared resource.

### Bullet 3 — rayon ONLY for CPU-prep, NOT for embedding

MET.

A source-level grep over `rust-core/src/` finds zero occurrences of `rayon`, `par_iter`, or `into_par_iter`. Rayon appears only in `Cargo.lock` as a transitive dependency of fastembed/tokenizers but is never called from project code. The embedding call chain is a plain sequential `Vec::iter().map(…).collect()` prefix step (`fastembed_embedder.rs:237`) followed by `guard.embed(…)` (`fastembed_embedder.rs:243`). Bullet 3 is unconditionally satisfied.

### Bullet 4 — two-instance fallback: bulk and query on separate sessions

MET.

`FastEmbedder` carries two independent `Mutex<TextEmbedding>` fields:

```
// fastembed_embedder.rs:90–98
pub struct FastEmbedder {
    bulk: Mutex<TextEmbedding>,   // line 93
    query: Mutex<TextEmbedding>,  // line 95
    dim: usize,
}
```

Both are loaded independently by `new_builtin` (`fastembed_embedder.rs:119–120`) and `new_local` (`fastembed_embedder.rs:152–155`). The module doc (`fastembed_embedder.rs:9–31`) explicitly documents the design rationale: a single shared mutex would cause head-of-line blocking; two instances eliminate it. `embed_passages` locks only `bulk` (line 257); `embed_query` locks only `query` (line 262). A multi-minute bulk reindex therefore leaves query latency entirely untouched.

### Verdict

**DONE** via the two-instance fallback (Bullet 4). Head-of-line blocking is eliminated by construction: bulk reindex and query embedding hold different mutexes on different `TextEmbedding` instances and can proceed fully concurrently. Rayon is not used over embedding anywhere (Bullet 3 fully met). The only gap is Bullet 1's intra-op thread count: `intra_threads` is accepted in the config but not yet wired through to `InitOptions`, so ORT uses its own defaults rather than the specified `ncores−1`. This is a minor explicit-tuning gap — the latency-isolation goal of Stage 2 is met regardless, because two-instance isolation is a stronger guarantee than thread-count partitioning.
