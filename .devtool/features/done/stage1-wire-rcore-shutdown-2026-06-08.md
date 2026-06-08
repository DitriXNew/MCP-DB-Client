---
id: "stage1-wire-rcore-shutdown-2026-06-08"
status: "done"
priority: "high"
assignee: null
dueDate: null
created: "2026-06-08T21:16:43.000Z"
modified: "2026-06-08T21:31:30.000Z"
completedAt: "2026-06-08T21:31:30.000Z"
labels: ["stage-1", "cpp-glue"]
order: "aS"
---

# Stage 1 — Привязать rcore_shutdown к doStopListen

Loose end, вскрытый аудитом потокобезопасности ([THREAD-SAFETY.md](http-1c-dll/THREAD-SAFETY.md), latent-risk #3) и заложенный в acceptance FFI-скелета: экспорт `rcore_shutdown` существует, но **не вызывается** ниоткуда.

## Acceptance
- [ ] Вызвать `rcore_shutdown()` из `doStopListen()` (остановка сервера/закрытие формы), **НЕ** из `~HttpServerComponent` (процессный синглтон переживает один экземпляр компонента)
- [ ] Порядок: stop-accept → дренаж pending → join `serverThread` → `rcore_shutdown()` (воркер джойнится до возможной выгрузки `onnxruntime.dll`)
- [ ] Идемпотентно (повторный вызов безопасен — `rcore_shutdown` это гарантирует)
- [ ] Build-verify: Release-сборка DLL компилируется и линкуется

Refs: §4.4, §9, THREAD-SAFETY.md latent-risk #3.
