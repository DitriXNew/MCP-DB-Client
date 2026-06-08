---
id: "stage1-search-dense-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-08T19:51:43.000Z"
completedAt: "2026-06-08T19:51:43.000Z"
labels: ["stage-1", "rust-core"]
order: "aB"
---

# Stage 1 — search (dense)

## Acceptance
- [ ] Поля: `query`, `collection?`, `k`, `min_score?`, `max_per_doc?`, `include_text`
- [ ] dense: top-k по скалярному произведению над нормализованными векторами
- [ ] Хиты: `{doc_id, name, collection, meta, segment_id, line_start?, line_end?, score, text|preview}`
- [ ] Query-эмбеддинг — на общей сессии, вне лока индекса
- [ ] Если `vector_status = building` → вернуть **частичный результат + флаг неполноты**
- [ ] Латентность — единицы–десятки мс на 5–10к (flat + dot product)

Refs: §5.3, §9.

## Implementation notes
`search` dispatch arm → `store::search` under a read lock. Embeds the query on the shared embedder (outside any index mutation), scores every segment that has a vector by dot product (== cosine, normalized), sorts descending, then applies `min_score`, `max_per_doc`, and `k`. `collection` scopes the search (all collections if omitted). Hits return `{doc_id, name, collection, meta, segment_id, line_start?, line_end?, score}` plus `text` when `include_text=true`, else a ~120-char `preview`. If any in-scope collection is still `Building`, the top-level `partial: true` flag is set and whatever vectors are ready are returned (partial result). Dense only — keyword/hybrid/meta-filters are later cards.
