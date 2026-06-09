---
id: "stage3-vanessa-step-registry-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T18:03:21.000Z"
modified: "2026-06-09T09:05:00.000Z"
completedAt: "2026-06-09T09:05:00.000Z"
labels: ["stage-3", "investigation", "blocker"]
order: "aM"
---

# Stage 3 — Доступ к реестру шагов Ванессы (откр. вопрос §11.5)

Блокер для каталога шагов QA-адаптера.

## Acceptance
- [x] Программно получить **полный каталог** доступных шагов Vanessa Automation
- [x] С описаниями и параметрами
- [x] Источник — реестр шагов Ванессы (НЕ Gherkin1C)

Refs: §7.1, §11.5.

---

## Findings (2026-06-09, subagent investigation of `D:\GitHub\vanessa-automation`)

The full step catalog lives as an **in-memory ValueTable `ТаблицаИзвестныхStepDefinition`**, populated at form init from the loaded step-library EPFs (`ДобавитьСнипетыСервер` → `ДобавитьСнипет` → `ДобавитьСнипетВТаблицуИзвестныхStepDefinitionССервера`, in `Forms/УправляемаяФорма/Ext/Form/Module.bsl` ~28459–28634).

Columns per step (the data we need):
- `ПредставлениеТеста` — **canonical step phrase** (= `ИмяШага` in steps.json)
- `ОписаниеШага` — **description** (= `ОписаниеШага`)
- `ТипШага` / tree `ПолныйТипШага` — **category** (= `ПолныйТипШага`)
- `Параметры` — array of `{Тип: Строка|Число|Дата}` **parameter type-hints** (inferred from the param name; no defaults/descriptions)
- `ID` — full id with params `"phrase(p1, p2)"`; `ИмяФайла` — source EPF; `Транзакция`, `ВерсияФайла`.

**Export = exactly steps.json:** `ПодготовитьТаблицуДляВыгрузкиШагов` (`Forms/ВыборИзвестногоШага/Ext/Form/Module.bsl` ~1157–1182) emits `{ИмяШага, ОписаниеШага, ПолныйТипШага}` — the precise shape of `test/steps.json`. It is triggered by the UI button `ВыгрузитьШагиВJSON` (manual), but the **table is readable programmatically at runtime**:

```bsl
Для Каждого СтрШага Из ТаблицаИзвестныхStepDefinition Цикл
    // phrase=ПредставлениеТеста, description=ОписаниеШага, type=ТипШага,
    // parameters=Параметры (массив с "Тип"), snippet_id=ID, file=ИмяФайла
КонецЦикла;
```

**Limitations:** no dedicated public API (read the table directly); params are type-hints only; catalog is loaded on demand from EPF libs (no standalone registry file). For the adapter's `embed_text` use `phrase | description | type | paramTypes`. Unblocks [[stage3-qa-adapter-segments]] (step-catalog half) and [[stage3-find-step-usages]].
