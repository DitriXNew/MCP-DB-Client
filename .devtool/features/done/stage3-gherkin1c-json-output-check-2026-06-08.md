---
id: "stage3-gherkin1c-json-output-check-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-09T09:05:00.000Z"
completedAt: "2026-06-09T09:05:00.000Z"
labels: ["stage-3", "investigation", "blocker"]
order: "aL"
---

# Stage 3 — Проверка JSON-выхлопа Gherkin1C (откр. вопрос §11.1)

Блокер для line-addressing QA-адаптера.

## Acceptance
- [x] Подтвердить, что выхлоп `lintest/Gherkin1C` несёт **позиции строк**
- [x] Различение элементов: scenario / структура сценария / предыстория (background) / теги / примеры (examples)
- [x] Если позиций нет — определить fallback для `line_start/line_end`

Refs: §7.1, §11.1.

---

## Findings (2026-06-09, subagent investigation of `D:\GitHub\vanessa-automation`)

**YES — the parser emits line numbers.** Every element carries a 1-indexed `.line` field (Cucumber-messages-like schema). Parser source: `vanessa-automation/VanessaAutomation/Forms/ПарсерGherkin/Ext/Form/Module.bsl` (reads JSON from the external component at ~lines 169–171; consumes `.line` at 265, 279, 312, 347, 555, 576, 594, 611, 621, 654, 692, 1000+).

Output shape (array of feature objects):
- `feature` {name, `comments[].line`, `description[].line`, `tags[].line`}
- `background` {`line`, `keyword`, `steps[].line`}
- `scenarios[]` {`line`, `keyword{text,type}`, name, `tags[].line`, `steps[].line`, optional `examples`}
- `examples` {`line`, `head.line`, `body[].line`, table `tokens[]`}
- steps may carry `params[]` and a `snippet` id.

**Element types are distinguished:** Background = the `background` object; Scenario vs Scenario Outline = presence of `.examples` (parser sets `ДопТип="СтруктураСценария"`); tags/examples have their own `.line`. So `line_start = scenario.line`, `line_end = next scenario.line − 1` (or EOF); element type from the JSON node.

**Verdict:** the QA adapter CAN rely on Gherkin1C for line addressing — call `ВнешняяКомпонентаПарсерGherkin.ПрочитатьФайл()` (async `НачатьВызовПрочитатьФайл`) and read `.line` per node. **Fallback** (component unavailable): the current `http1c` BSL block-scanner already reconstructs scenario blocks; add a running `LineNumber` counter to capture `line_start`/`line_end` while scanning. Unblocks [[stage3-qa-adapter-segments]].
