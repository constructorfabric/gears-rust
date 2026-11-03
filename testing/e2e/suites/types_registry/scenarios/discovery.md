# Discovery scenarios (`GET /entities`)

End-to-end discovery scenarios cover namespace patterns, the `kind`, `depth` and lifecycle filters, projection, cursor continuation and hydration through `:batchGet`. The local launcher uses **SQLite**; these tests do not prove PostgreSQL/MySQL or tenant/PDP behavior.

Every scenario has an e2e test in [test_discovery.py](../test_discovery.py) or [test_discovery_patterns.py](../test_discovery_patterns.py); the [suite README](../README.md) lists each group. Each scenario links its source fixtures and states the registered data, requests and expected results; all fixture links point to `fixtures/discovery/`. Every scenario registers into a fresh target namespace. Namespace-isolation scenarios additionally register neighbouring controls; identifier-matching scenarios use version, name, kind and chain controls within the target namespace.

Unless a scenario says otherwise, discovery lists only active entities, a walk follows every `next_cursor` with the same query until none remains, and it returns each matching entity once in canonical GTS-ID order. Items are projected as an exact read projects them: the mandatory `gts_id`, `gts_uuid`, `kind` and `lifecycle_status` accompany every selection, an absent `$select` returns the document-free default metadata, and a field that does not apply to an item's kind is absent. Pages carry no validators.

## Contents

- [Paged discovery and returned fields](#paged-discovery-and-returned-fields)
  - [TR-DISC-001 — Limit-one cursor visits every row exactly once](#tr-disc-001--limit-one-cursor-visits-every-row-exactly-once)
  - [TR-DISC-002 — Pattern excludes neighbouring namespaces](#tr-disc-002--pattern-excludes-neighbouring-namespaces)
  - [TR-DISC-003 — Content projection survives page continuation](#tr-disc-003--content-projection-survives-page-continuation)
  - [TR-DISC-004 — Inapplicable selected artifacts remain absent on Instances](#tr-disc-004--inapplicable-selected-artifacts-remain-absent-on-instances)
- [Filters](#filters)
  - [TR-DISC-101 — Kind selects Type Schemas or Instances](#tr-disc-101--kind-selects-type-schemas-or-instances)
  - [TR-DISC-102 — Pattern, depth, kind, and selection compose](#tr-disc-102--pattern-depth-kind-and-selection-compose)
  - [TR-DISC-103 — Lifecycle filter separates live rows and tombstones](#tr-disc-103--lifecycle-filter-separates-live-rows-and-tombstones)
  - [TR-DISC-104 — Type scopes, version wildcards and token boundaries](#tr-disc-104--type-scopes-version-wildcards-and-token-boundaries)
  - [TR-DISC-105 — Pinned type minors and major zero](#tr-disc-105--pinned-type-minors-and-major-zero)
  - [TR-DISC-106 — Concrete instance structure and base-minor flexibility](#tr-disc-106--concrete-instance-structure-and-base-minor-flexibility)
  - [TR-DISC-107 — Later wildcards and beginning-of-ID anchoring](#tr-disc-107--later-wildcards-and-beginning-of-id-anchoring)
  - [TR-DISC-108 — Three segments pin independently](#tr-disc-108--three-segments-pin-independently)
  - [TR-DISC-109 — Unregistered UUID selectors remain valid](#tr-disc-109--unregistered-uuid-selectors-remain-valid)
- [Cursor continuation](#cursor-continuation)
  - [TR-DISC-201 — A changed pattern cannot reuse a cursor](#tr-disc-201--a-changed-pattern-cannot-reuse-a-cursor)
  - [TR-DISC-202 — Page size may change without changing cursor identity](#tr-disc-202--page-size-may-change-without-changing-cursor-identity)
- [Read discovered entities with batchGet](#read-discovered-entities-with-batchget)
  - [TR-DISC-301 — Discovered references can hydrate the complete set through batchGet](#tr-disc-301--discovered-references-can-hydrate-the-complete-set-through-batchget)

## Paged discovery and returned fields

### TR-DISC-001 — Limit-one cursor visits every row exactly once

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register independent `person_schema` and `device_schema` Type Schemas in the target namespace. In a neighbouring namespace register `person_schema`/`person_instance` and `device_schema`/`device_instance` as distinct control pairs. Both target schemas are active.

**When:** GET with the target namespace pattern and `limit=1`, then follow every cursor with the same pattern and limit.

**Then:** exactly two pages of one item each: the first has a `next_cursor`, the second none, and the IDs appear once each in canonical order.

### TR-DISC-002 — Pattern excludes neighbouring namespaces

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active. Register a second `person_schema` with a distinct namespace prefix.

**When:** discover using the first namespace's pattern, then the pattern formed by appending `*` to `employee_schema`'s `gts_id`, then a valid pattern for an unregistered name in the first namespace.

**Then:** the namespace walk contains exactly its 12 entities and excludes the neighbouring schema. The employee branch walk contains exactly `employee_schema`, `manager_schema`, `director_schema`, `employee_instance`, `manager_instance`, and `director_instance`; the contractor branch, direct person Instance and device tree stay outside it. The non-matching pattern returns an empty `items` array with no cursor, not a `404`.

### TR-DISC-003 — Content projection survives page continuation

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active.

**When:** Traverse the target namespace pattern with `limit=1&$select=content` on every page.

**Then:** each item contains only the mandatory `gts_id`, `gts_uuid`, `kind`, and `lifecycle_status` and its whole authored `content`; no `origin` or other document appears. All 12 fixture documents survive the cursor walk unchanged, including derived schemas' authored `$ref` values and deeply derived Instance payloads.

### TR-DISC-004 — Inapplicable selected artifacts remain absent on Instances

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active.

**When:** Discover using the target namespace pattern and `$select=resolved_schema`.

**Then:** all six Type Schema items have a nonempty `resolved_schema` object; all six Instance items have `kind=instance` and omit `resolved_schema`, at every depth. Each item carries the mandatory `gts_id`, `gts_uuid`, `kind`, and `lifecycle_status`.

## Filters

### TR-DISC-101 — Kind selects Type Schemas or Instances

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active.

**When:** Traverse the target namespace separately with `kind=type_schema` and `kind=instance`, each at `limit=2`.

**Then:** the first walk returns exactly the six schema fixtures and the second exactly the six Instance fixtures, each in canonical order regardless of depth or branch. Each page contains only its requested kind, with default metadata fields and no authored content.

### TR-DISC-102 — Pattern, depth, kind, and selection compose

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active. In a second namespace register `person_schema`, then `employee_schema` and `employee_instance`, rewriting their IDs and `$ref` to that namespace.

**When:** discover with `pattern` equal to `employee_schema`'s `gts_id` followed by `*`, `depth=3`, `kind=instance`, and `$select=content`.

**Then:** only the target namespace's `employee_instance` appears, with its authored content and mandatory fields. The neighbouring employee Instance and local contractor Instance test pattern exclusion, the local employee and manager schemas test kind exclusion, and the local manager Instance tests depth exclusion independently. A `depth=2` repeat returns an empty result and terminates.

### TR-DISC-103 — Lifecycle filter separates live rows and tombstones

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register `person_schema` and `device_schema` as independent target schemas, then delete `person_schema` and await completion; `device_schema` remains active. In a neighbouring namespace register `person_schema`/`person_instance` and `device_schema`/`device_instance` as distinct control pairs.

**When:** Discover using the target namespace pattern with absent lifecycle, `active`, `deleted`, and `all`.

**Then:** absent and `active` return only the target live schema, `deleted` only the target tombstone, and `all` both target schemas in canonical order. No neighbouring control appears. The tombstone remains exact-readable.

### Identifier matching examples

TR-DISC-104–109 exercise GTS §10 through `GET /entities?pattern=…`. Each example below states the rule, the registered IDs and the exact scope of the result.

**Notation and common checks**

- IDs and patterns omit the initial `gts.{ns}.` unless they explicitly start with `gts.`. `{ns}` is the test's unique namespace, for example `cf.e2e.r123`. Thus `person.v1~{ns}.alice.v1` stands for `gts.cf.e2e.r123.person.v1~cf.e2e.r123.alice.v1`.
- In a tree, append each child label to its parent's full ID. A trailing `~` identifies a type schema; the other leaves are instances.
- Every listed entity must register successfully and is active during discovery. Controls are registered entities that must stay outside a selector's result.
- Each query follows all cursors at `limit=2`. Tests compare complete metadata pages, canonical GTS-ID order, and termination. Counts below describe the whole walk, not one page. An empty result is `200` with `items=[]` and no cursor.

### TR-DISC-104 — Type scopes, version wildcards and token boundaries

**Rule:** An omitted minor accepts every minor of the same major. `type~` and `type~*` select the same scope. `v2.*` requires a minor in that segment; a later segment's minor does not count.

**Registered IDs:**

```text
person.v0.0~
person.v0.1~
person.v1.0~
├── {ns}.alice.v1
├── {ns}.employee.v1.0~
│   └── {ns}.item.v2
└── {ns}.employee.v1.1~
    └── {ns}.item.v2
person.v1.1~
person.v1.2~
person.v2~
├── {ns}.alice.v1
└── {ns}.employee.v1.0~
person.v10~
└── {ns}.alice.v1
person_extra.v1~
```

**Queries and expected results:**

| Pattern | Expected IDs | Count |
|---|---|---:|
| `person.v1~`, `person.v1~*`, `person.v1.*` | The three `person.v1` roots and all five descendants under `person.v1.0~` | 8 |
| `person.*`, `person.v*` | Every ID above except `person_extra.v1~` | 15 |
| `person.v2.*` | None: `person.v2~` has no minor, even though its employee type has minor 0 | 0 |
| `person.v2~*` | `person.v2~` and its two descendants | 3 |
| `person.v2~{ns}.*` | Only the two descendants of `person.v2~` | 2 |

`person.v10~` and its instance check the major boundary; `person_extra.v1~` checks the type-name boundary.

**Fixtures and tests:** [person_schema](../fixtures/discovery/person_schema.json), `version_examples` in [test_discovery_patterns.py](../test_discovery_patterns.py). Eight selector cases.

### TR-DISC-105 — Pinned type minors and major zero

**Rule:** An explicit minor constrains only its own segment. Major zero follows the same matching rules.

**Registered IDs:** The `version_examples` dataset shown in TR-DISC-104. Its relevant subtrees are:

```text
person.v0.0~
person.v0.1~
person.v1.0~
├── {ns}.alice.v1
├── {ns}.employee.v1.0~
│   └── {ns}.item.v2
└── {ns}.employee.v1.1~
    └── {ns}.item.v2
```

The other registered root minors and majors are exclusion controls. There are no instances under major-zero types because managed admission forbids them.

**Queries and expected results:**

| Pattern | Expected IDs | Count |
|---|---|---:|
| `person.v1.0~`, `person.v1.0~*` | `person.v1.0~` and its five descendants, including `employee.v1.1~` and both `item.v2` instances | 6 |
| `person.v0~*`, `person.v0.*` | `person.v0.0~`, `person.v0.1~` | 2 |
| `person.v0.1~*` | `person.v0.1~` | 1 |

**Fixtures and tests:** [person_schema](../fixtures/discovery/person_schema.json), `version_examples` in [test_discovery_patterns.py](../test_discovery_patterns.py). Five selector cases.

### TR-DISC-106 — Concrete instance structure and base-minor flexibility

**Rule:** A concrete instance selector can flex omitted minors, but must preserve names, majors, kind and chain length.

**Registered matching instances:**

```text
person.v1.0~{ns}.alice.v1
person.v1.1~{ns}.alice.v1
person.v1.2~{ns}.alice.v1
```

**Registered controls:**

| IDs | What they check |
|---|---|
| `person.v1.0~{ns}.alice.v10` | Instance major 1 must not match major 10 |
| `person.v1.0~{ns}.alice_extra.v1` | Instance names must match completely |
| `person.v2~{ns}.alice.v1`, `person.v10~{ns}.alice.v1` | The base major must also match exactly |
| `person.v1.0~{ns}.employee.v1.0~`, `person.v1.2~{ns}.employee.v1~` | A same-name type cannot satisfy an instance selector |
| Each employee type above followed by `{ns}.item.v2` | A longer chain cannot satisfy that instance selector |

The five base type schemas are registered too. All stored instances have a major-only final segment, as required by managed admission.

**Queries and expected results:**

| Pattern | Expected IDs | Count |
|---|---|---:|
| `person.v1~{ns}.alice.v1` | All three matching instances listed above | 3 |
| `person.v1~{ns}.employee.v1` | None: employee types and their instances have the wrong kind or chain length | 0 |
| `person.v1~{ns}.employee.v1.0` | None, including the type ending in `employee.v1.0~` | 0 |
| `person.v1.0~{ns}.alice.v1` | `person.v1.0~{ns}.alice.v1` | 1 |
| `person.v1~{ns}.alice.v1.0` | None: an explicit instance minor excludes major-only tails | 0 |
| `person.v1.0~{ns}.alice.v1.0` | None for the same reason, even with the base minor pinned | 0 |

**Fixtures and tests:** [person_schema](../fixtures/discovery/person_schema.json), `instance_examples` in [test_discovery_patterns.py](../test_discovery_patterns.py). Six selector cases.

### TR-DISC-107 — Later wildcards and beginning-of-ID anchoring

**Rule:** In a later segment, `v1.*` requires an explicit minor; `v1~*` also accepts an omitted minor. Matching always starts at the beginning of the ID.

**Registered branch dataset:**

```text
person.v1.0~
├── {ns}.alice.v1
├── {ns}.employee.v1.0~
│   └── {ns}.item.v2
└── {ns}.employee.v1.1~
    └── {ns}.item.v2
person.v1.1~
├── {ns}.employee.v1.0~
│   └── {ns}.item.v2
└── {ns}.employee.v1.1~
    └── {ns}.item.v2
person.v1.2~
└── {ns}.employee.v1~
    └── {ns}.item.v2
```

**Queries against this dataset:**

| Pattern | Expected IDs | Count |
|---|---|---:|
| `person.v1~{ns}.employee.v1.*` | The four explicit-minor employee types and their four `item.v2` instances | 8 |
| `person.v1~{ns}.employee.v1~*`, `person.v1~{ns}.employee.v1~` | All five employee types and their five `item.v2` instances | 10 |
| `person.v1~{ns}.employee.v1.0~` | The two employee types with minor 0, under base minors 0 and 1, plus their two instances | 4 |
| `gts.{ns}.employee.v1~` | None: every employee type is a later segment under `person` | 0 |

**Namespace wildcard:** A separate query uses the `version_examples` dataset from TR-DISC-104, so other first-segment majors act as controls.

| Pattern | Expected IDs | Count |
|---|---|---:|
| `person.v1~{ns}.*` | The direct Alice instance, both employee types, and both `item.v2` instances under `person.v1.0~`; no base roots or entities under other majors | 5 |

**Fixtures and tests:** [person_schema](../fixtures/discovery/person_schema.json), `employee_branches` and `version_examples` in [test_discovery_patterns.py](../test_discovery_patterns.py). Six selector cases.

### TR-DISC-108 — Three segments pin independently

**Rule:** Pinning the base minor, derived minor or both narrows only those segments. Other omitted minors remain flexible.

**Registered instance candidates:** The branch dataset in TR-DISC-107 contains these five `item.v2` instances:

```text
person.v1.0~{ns}.employee.v1.0~{ns}.item.v2
person.v1.0~{ns}.employee.v1.1~{ns}.item.v2
person.v1.1~{ns}.employee.v1.0~{ns}.item.v2
person.v1.1~{ns}.employee.v1.1~{ns}.item.v2
person.v1.2~{ns}.employee.v1~{ns}.item.v2
```

Their parent schemas and the direct Alice instance are registered as controls.

**Queries and expected results:**

| Pattern | Expected IDs from the list above | Count |
|---|---|---:|
| `person.v1~{ns}.employee.v1~{ns}.item.v2` | All five instances, including the one whose employee minor is omitted | 5 |
| `person.v1~{ns}.employee.v1.0~{ns}.item.v2` | The two instances under `employee.v1.0~`, with base minors 0 and 1 | 2 |
| `person.v1.0~{ns}.employee.v1~{ns}.item.v2` | The first two instances: base minor 0, derived minor 0 or 1 | 2 |
| `person.v1~{ns}.employee.v1~{ns}.item.v2.0` | None: every stored instance ends in major-only `item.v2` | 0 |
| `person.v1.0~{ns}.employee.v1.0~{ns}.item.v2` | Only `person.v1.0~{ns}.employee.v1.0~{ns}.item.v2` | 1 |

**Fixtures and tests:** [person_schema](../fixtures/discovery/person_schema.json), `employee_branches` in [test_discovery_patterns.py](../test_discovery_patterns.py). Five selector cases.

### TR-DISC-109 — Unregistered UUID selectors remain valid

**Rule:** A UUID-tail selector is valid even when no matching entity is registered. It must not select named instances.

**Registered IDs:** The complete `instance_examples` dataset from TR-DISC-106, including the base schemas, named Alice instances, employee types, longer chains and name/major controls. No UUID-tail entity is registered: managed admission rejects explicit UUID tails.

Here `{uuid}` means `7a1d2f34-5678-49ab-9012-abcdef123456`.

| Pattern | Expected IDs | Count |
|---|---|---:|
| `person.v1~{uuid}` | None | 0 |
| `person.v1.0~{uuid}` | None | 0 |
| `person.v2~{uuid}` | None | 0 |

These cases prove selector acceptance and exclusion of named entities. They do not prove positive UUID-tail matching.

**Fixtures and tests:** [person_schema](../fixtures/discovery/person_schema.json), `instance_examples` in [test_discovery_patterns.py](../test_discovery_patterns.py). Three selector cases.

### Matching coverage and admission limits

The six scenarios above contain 33 selectors corresponding to GTS specification §10 and `gts-spec/tests/shared_id_pattern_cases.py` (`MATCH_CASES`).

| GTS matching cases | HTTP scenario |
|---|---|
| `type`, `all_majors`, `first_minor_wildcard` | TR-DISC-104 |
| `pinned_minor`, `v0_chain`, `v0_minor_wildcard`, `v0_pinned` | TR-DISC-105 |
| `instance`, `instance_pinned` (admissible subset) | TR-DISC-106 |
| `later_minor_wildcard`, `later_chain_wildcard`, `derived_type`, `anchored_type`, chain prefix | TR-DISC-107 |
| `chain_instance`, pinned base/derived/instance selectors (admissible subset) | TR-DISC-108 |
| UUID selector acceptance / empty selection | TR-DISC-109 |

Managed admission limits which specification candidates can be registered:

| Admission restriction | Coverage consequence |
|---|---|
| A schema major cannot mix major-only and minor-bearing spelling | Use different majors or different preceding type IDs to demonstrate the two shapes |
| A version family holds one kind | Same-family type/instance distinctions cannot be set up together |
| Registered instances require a major-only final segment | Explicit-minor instance selectors can be tested for exclusion, but not positive instance-minor matching |
| Explicit UUID tails are rejected | Positive UUID-tail matching remains a lower-level test responsibility |
| Major-zero types cannot have registered instances | Major-zero discovery examples contain only schemas |

Selector-to-selector coverage (`COVERAGE_CASES`) is outside discovery: stored entities have concrete identifiers. Exhaustive invalid syntax and numeric/length bounds also remain lower-level checks. These HTTP examples do not claim complete specification coverage.

## Cursor continuation

### TR-DISC-201 — A changed pattern cannot reuse a cursor

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register independent `person_schema` and `device_schema` Type Schemas in the target namespace. In a neighbouring namespace register `person_schema`/`person_instance` and `device_schema`/`device_instance` as distinct control pairs. Obtain a first page at `limit=1` under the target namespace pattern, with a nonempty cursor.

**When:** Resume the cursor with the neighbouring namespace pattern, keeping `limit=1` and the original selection.

**Then:** `400` Problem JSON rejects the continuation rather than mixing traversals.

### TR-DISC-202 — Page size may change without changing cursor identity

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [device_schema](../fixtures/discovery/device_schema.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Generate independent closed-envelope roots from `person_schema`, rewriting each `gts_id` and authored `$id` consistently. Register four target roots named `item000` through `item003` in one namespace; their canonical order is known before reading. In a neighbouring namespace register `person_schema`/`person_instance` and `device_schema`/`device_instance` as distinct control pairs.

**When:** request the first page with `limit=1`, the namespace pattern and `$select=content`. Resume its cursor with `limit=2`, preserving the pattern and selection, and continue with limit 2 until no cursor remains.

**Then:** the first page reports limit 1 and contains exactly the first expected entity. The next request succeeds with `200`, reports limit 2 and contains exactly the next two expected entities; subsequent pages contain at most two items and report limit 2. Every item retains the requested projection and expected authored content. The completed walk returns all four entities once in canonical order and terminates. Changing page size neither invalidates the cursor nor resets or skips its position.

## Read discovered entities with batchGet

### TR-DISC-301 — Discovered references can hydrate the complete set through batchGet

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active. Its 12 GTS IDs and authored documents are known from these fixtures, and no mutation occurs during the reads.

**When:** traverse namespace-scoped discovery with `limit=1` and no `$select`. Collect each item's `gts_uuid` in discovery order and submit those returned references directly as batchGet keys, with top-level `"$select": "content"`. Do not derive UUIDs or substitute fixture GTS IDs for the discovered references.

**Then:** discovery returns exactly the 12 expected entities in canonical GTS-ID order, with default metadata and no documents or validators, and the walk terminates. BatchGet returns `200` with 12 `found` results, each carrying an `etag` and echoing its UUID key in canonical lowercase hyphenated form. Match results by key after checking completeness and uniqueness; batch response order is not contractual. Each entity has exactly its mandatory fields and authored `content`; its identity matches the corresponding discovery item and its content matches the independent fixture. Every discovered reference resolves, and no expected entity is missing or duplicated in the hydrated result.
