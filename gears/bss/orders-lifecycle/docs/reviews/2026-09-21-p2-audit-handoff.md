# P-2 audit review handoff

Date: 2026-09-21. Updated after D-105 remediation. Scope: Orders design through D-105, including Foundation,
read/authz, Capture, DESIGN, DECISIONS, ADR-0001 and upstream identity requirements.
This is a documentation review, not runtime verification or reviewer approval.

## Disposition

Review completed; **P-2 remains pending reviewer acceptance and implementation evidence**.
The four previous consistency findings and P2-H1/P2-M1 now have documented corrections. The
original findings below are retained as review history, not open design findings. Orders deliberately
chooses Pricing-style local transactional audit instead of the original event-only recommendation.
Acceptance of that alternative remains with the reviewer/architect. OL-14/OL-15 are not closed
by pretending the requested machinery was removed; OL-13's remaining contract work is tracked
below without claiming its original wording was independently revalidated in this pass.

## Findings addressed by D-105

### P2-H1 — High: create is specified as a special result, not a complete execution branch

**Status: addressed in design by D-105.** Foundation §3.6 now dispatches create to its own
authorization/idempotency/guard/insertion/audit transaction branch; Capture delegates allocation.
Successful replay returns the committed identity/number, and refusals create no placeholder.
The historical evidence below describes the pre-fix state; concurrency/crash tests remain owed.

Evidence: [Foundation Attempt Transition](../design/01-foundation.md#transition-commit) still
loads/locks an existing aggregate at step 5, looks up current state at step 10 and captures
outgoing state at step 16. [Capture Create Draft](../design/02-capture.md#create-draft-order)
assigns an identity and delegates to that engine; it does not insert an aggregate beforehand.
D-104 specifies the correct committed row (NULL prior state, version/sequence 1) but does not
explain how creation bypasses those existing-row assumptions. Guard-input refusal step 3.1 has
the same unconditional aggregate load.

Impact: implementation must invent the creation transaction ordering, potentially creating a
placeholder aggregate to audit a refused create or breaking same-key create replay.

Required resolution: specify the create branch through authorization, idempotency, guard
refusal, aggregate/version insertion and audit append. Failed create must persist its allowed
refusal evidence without a placeholder order; successful create must commit aggregate, version 1,
audit sequence 1 and idempotency outcome atomically. This is a remaining execution-contract gap,
not a regression in the newly corrected NULL-state schema.

### P2-M1 — Medium: audit-read narrative promises rows the new policy intentionally hides

**Status: addressed in design by D-105.** Diagram and description now return the authorized
subset and explicitly exclude an exhaustive-denial promise. The additional operational
permission remains required; no role was broadened. Evidence below describes the pre-fix state.

Evidence: [Audit retrieval](../design/08-read-and-authz.md#audit-retrieval) diagram returns a
trail “including denied authorization attempts,” and the introductory description is similarly
unqualified. D-104's following paragraph correctly excludes unresolved rows unless the reader
also has explicit subject-tenant-scoped `audit-unresolved × read` permission; ordinary customer,
partner and seller roles do not receive it by default.

Impact: tests or clients following the diagram could expect complete denial visibility under
ordinary order access, or implementations could accidentally bypass the additional permission.

Required resolution: qualify the diagram/description as the currently authorized subset and
state that an order trail is not an exhaustive operational-denial listing. Preserve the narrow
permission rather than granting access merely to make the diagram true.

## D-104 regression check

| Previous finding | Current contract | Result |
|------------------|------------------|--------|
| Editable draft resource tenant vs frozen chain | Aggregate `audit_tenant_id` remains immutable; business resource tenant is a separate row snapshot; checkpoints enumerate immutable namespaces | Corrected at design level; draft-edit/authorization tests still required |
| Unresolved refusals lack scope | Mandatory trusted `subject_tenant_id`, scoped service append, explicit operational read and subject-scoped index | Corrected at design level; catalog/PDP/ORM bindings must be implemented and tested |
| Create cannot represent prior state | Create-specific NULL `from_state`, draft target, version/sequence 1; other committed states remain non-null | Schema corrected; D-105 also specifies the execution branch for P2-H1 |
| Blanket engine-only rule conflicts with checkpoints | Foundation §2.2/§4.1 and ADR-0001 distinguish engine evidence, checkpoint appends, SELECT-only verification and bounded retention | Corrected at design level |

Hash review: the row preimage now covers `audit_tenant_id`, `subject_tenant_id` and the separate
business `resource_tenant_id`; genesis uses audit namespace plus order ID. Checkpoint header,
members, genesis and digest consistently use the audit namespace. Identity/profile resolution
is absent from hashing. D-104 explicitly prohibits rehashing deployed evidence under a silently
changed v1 format. Frozen vectors and database round-trip tests remain outstanding.

## Proposed response to the original reviewer

Предлагаем альтернативное решение P-2: оставить локальный транзакционный аудит по архитектурному
паттерну Pricing. Аудит-запись фиксируется в одной транзакции с изменением заказа; Orders сохраняет
append-only хранилище, цепи по агрегатам и авторизованное чтение. Переписывание акторов и пересчёт
исторических цепей при удалении идентичности исключены: сохраняем subject ID из SecurityContext,
а идентификационные данные остаются ответственностью IdP.

Как в дизайне Pricing, предусмотрены периодическая верификация, tenant roll-up и необязательное
внешнее закрепление. Это проектные обязательства: в проверенной реализации Pricing verifier и
roll-up ещё отсутствуют. Не заявляем, что платформа уже предоставляет эти механизмы.

Специфика Orders сохранена: поля перехода и делегирования, аудит системных переходов, отдельная
политика отказов/read-access log, собственные правила доступа и явно live-пагинация. Простое
enqueue в outbox обеспечивает атомарную постановку события, но не заменяет долговременный
проверяемый журнал. Поэтому OL-14/OL-15 предлагаем пересмотреть на основании существующего
паттерна Pricing, а не считать выполненными по исходному event-only варианту. Create-ветка и
описание видимости отказов уточнены в D-105. Для закрытия всё ещё необходимо согласие reviewer
на альтернативу; реализация и её тесты остаются отдельной работой.

## Work remaining, by category

**Design/review closure:** P2-H1/P2-M1 addressed by D-105; obtain reviewer agreement on the local-audit
alternative. Commercial retention Q-07 and other existing policy questions remain separate.

**Orders implementation:** tables/constraints/indexes/triggers; transactional writer and create
branch; registered scoped permissions; trusted scheduler identities; canonical encoding/vectors;
checkpoint capture, rolling verification and alerts; bounded retention; D-101 pagination. Test
rollback, concurrency, tenant changes, unauthorized denial reads, NULL-create shape, deletion and
tamper detection, profile disappearance, worker grants and checkpoint failure/capacity. Optional
anchoring requires deployment/provider configuration and tests only when selected. No shared
toolkit implementation is claimed to exist and no Pricing-internal code dependency is introduced.

**Shared platform follow-ups:** subject uniqueness/non-reuse across issuers/migration; IdP
deletion/disablement/session/cache/backup/restore behavior; applicable privacy and retention
approval. D-103 tracks these as a shared p2 identity follow-up, not an Orders-only p1 gate or a
verified guarantee. Orders still must configure valid service principals and reject known unsafe
identity configurations. No external messages, tickets or approvals were issued by this review.

## Validation

TOCs of the six checked design/decision/ADR/upstream files validate. `git diff --check` passes.
These structural checks do not prove the semantic fixes and are not substitutes for runtime tests.

The original review added only this handoff. The subsequent user-requested D-105 remediation
updated Foundation, Capture, read/authz and decision/status references; no runtime code changed.
