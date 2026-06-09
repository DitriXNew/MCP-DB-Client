---
id: "perf-bsl-append-log-strjoin-2026-06-09"
status: "todo"
priority: "medium"
assignee: null
dueDate: null
created: "2026-06-09T12:00:00.000Z"
modified: "2026-06-09T12:00:00.000Z"
completedAt: null
labels: ["perf", "1c-bsl"]
order: "a6"
---

# Perf — BSL: append-лог вместо O(N²) перезаписи + СтрСоединить вместо +-в-цикле

Квадратичные паттерны в [Module.bsl](http-1c-dp/http1c/Forms/Form/Ext/Form/Module.bsl).

- `WriteSelfTestResult`: на **каждую** строку лога — полная переконкатенация всего лога через `+` в цикле **и** truncate+перезапись всего файла → `O(N²)` по I/O и по строкам — [Module.bsl:3004](http-1c-dp/http1c/Forms/Form/Ext/Form/Module.bsl#L3004). Рядом `TraceLine` уже умеет append (`, , True`).
- `+`-в-цикле в `GetMetadataSummaryOnServer` ([Module.bsl:1824](http-1c-dp/http1c/Forms/Form/Ext/Form/Module.bsl#L1824)) и `JoinRagLines` ([Module.bsl:2496](http-1c-dp/http1c/Forms/Form/Ext/Form/Module.bsl#L2496)) — над уже готовыми массивами.
- (Опц.) двойная сериализация в `SendMCPResponse` — `body` сериализуется строкой, потом весь конверт сериализуется снова, ре-эскейпя внутренний JSON ([Module.bsl:2143](http-1c-dp/http1c/Forms/Form/Ext/Form/Module.bsl#L2143)); подтвердить контракт компонента.

## Acceptance
- [ ] `SelfTestAppend` пишет в append-режиме `TextWriter(..., , , True)`, без полной перезаписи файла
- [ ] `GetMetadataSummaryOnServer` / `JoinRagLines` → один `СтрСоединить(Lines, Символы.ПС)` вместо цикла
- [ ] (Опц.) подтвердить контракт `SendResponse` и убрать двойную сериализацию, если допустимо
