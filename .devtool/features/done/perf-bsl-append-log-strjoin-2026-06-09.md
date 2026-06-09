---
id: "perf-bsl-append-log-strjoin-2026-06-09"
status: "done"
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
- [x] `SelfTestAppend` пишет **только новую строку** в append-режиме (`TextWriter(file, UTF8, Chars.LF, Not IsFirst)` + `WriteLine`); первая строка усечает (Append=False), дальше дописывает — как `TraceLine`. Убраны и переконкатенация всего лога, и truncate+rewrite файла. Процедура `WriteSelfTestResult` удалена
- [x] `GetMetadataSummaryOnServer` / `JoinRagLines` → `StrConcat(Lines, Chars.LF) + Chars.LF` (модуль на английской локали) вместо `+`-в-цикле
- [~] (Опц.) двойная сериализация в `SendMCPResponse` — **НЕ трогал**: требует подтверждения контракта компонента (ждёт ли он `body` строкой или объектом), риск сломать проволочный формат ради опционального пункта. Оставлено как осознанный остаток

## Done 2026-06-09 (валидировано контуром)
Три квадратичных/циклических паттерна убраны. Прогон 1С-контура на свежей `.epf` (build-18): `DESIGNER /Load` ок (525678 байт), `RESULT: ALL PASS (6/6)` + `DONE`. Файл результата собран **новым append-режимом** в правильном порядке (STARTED→6 кейсов→RESULT→DONE), без дублей/обрывов, кириллица цела — то есть append-лог проверен end-to-end. Версия в логе = `selftest-build-18-perf-appendlog` (свежесть .epf подтверждена).
