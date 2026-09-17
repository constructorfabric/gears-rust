<!-- Created: 2026-09-17 by Constructor Tech -->
<!-- Updated: 2026-09-17 by Constructor Tech -->

# Feature: Search & Discoverability

- [ ] `p2` - **ID**: `cpt-cf-settings-service-featstatus-search-discoverability`

- [ ] `p2` - `cpt-cf-settings-service-feature-search-discoverability`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Search Settings](#search-settings)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Query Validation and Pattern](#query-validation-and-pattern)
  - [Corpus by Classification](#corpus-by-classification)
  - [Matched-Field Attribution](#matched-field-attribution)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Search Surface](#search-surface)
  - [Classification-Aware Corpus](#classification-aware-corpus)
  - [Hits](#hits)
  - [Dialect](#dialect)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

One query reaches any setting without learning the category tree: `GET /settings-service/v1/search?q=…` matches a setting's key, description, category name, Schema Default and the overrides explicitly set inside the caller's visible subtree, and answers a flat list of hits, each naming its category, the field that matched, and — for a value match — the scope where that value is set.

### 1.2 Purpose

Search is the primary discoverability mechanism the PRD asks for, and the one read path where output masking is **not** enough. Whether a match exists, how many there are, and what a hit shows each disclose content on their own; so the corpus is filtered by classification and authorization **before** matching. A `secret` value is never matched: searching secret content is unsupported, not masked. A `pii` value is matched only for a caller entitled to read it unmasked. An override is matched only where the caller could already read it: inside the target's non-standalone subtree. A `hidden` setting is absent, as it is absent from browsing.

Search runs over stored rows, not resolved values. An inherited value is a hit at the ancestor that set it, not at every scope that inherits it, and a Schema Default is a hit on its declaration, not at every scope that falls back to it. "What is in effect for tenant T" is a read, not a search; this bounds the work to indexed row scans.

**Requirements**: `cpt-cf-settings-service-fr-search-discoverability`, `cpt-cf-settings-service-nfr-scope-isolation`

**Principles**: `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Searches the whole catalogue at platform scope; overrides from every non-standalone tenant are in the corpus |
| `cpt-cf-settings-service-actor-tenant-admin` | Searches within its subtree; overrides outside it, and those of standalone descendants, are outside the corpus |
| `cpt-cf-settings-service-actor-authz-resolver` | Decides `read` on the value resource, narrowing the declarations the page may hold, and `read_unmasked`, which admits `pii` content to the corpus |
| `cpt-cf-settings-service-actor-tenant-resolver` | Supplies the target's non-standalone descendants, which bound the override corpus |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.3 Search & Discoverability
- **Design**: [DESIGN.md](../DESIGN.md) — §4.2 (Component: Search), §4.3 (REST API — Search, History & Preferences), §4.7 (the classification-split trigram index pairs on `setting_declarations.default_value` and `setting_values.value`), §3 (portability: value search degrades on SQLite)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.11
- **Dependencies**: entry 2.5, whose browse path this feature mirrors and whose resolver supplies the root tenant, the hierarchy and effective access; entry 2.7, whose `hidden` access excludes a setting from every administrative read; entry 2.2, whose categories are the breadcrumb
- **Not applicable**: Licence gating of the corpus — the License Resolver does not exist yet; the seam is the same one browse leaves open (R2). Domain-affinity filtering of hits beyond the domain visibility browse already applies (R3). Snippets and relevance ranking: the contract returns the matched field, not the matched text.

## 2. Actor Flows (CDSL)

### Search Settings

- [ ] `p2` - **ID**: `cpt-cf-settings-service-flow-search-discoverability-search`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- A flat page of hits: declaration-level hits for a key, description, category name or Schema Default match, carrying no scope; one hit per matching override, naming the scope where it is set

**Error Scenarios**:
- `q` shorter than two characters after trimming, or longer than two hundred
- An OData `$filter`, `$orderby` or `$select`, which the resource does not take
- The target outside the caller's subtree, or a standalone descendant
- A cursor minted for a different query, target or corpus

**Steps**:
1. [ ] - `p2` - Actor sends GET /settings-service/v1/search with `q`, optional `tenant`, `limit` and `cursor`; **IF** `q` trimmed is shorter than two characters or longer than two hundred → **RETURN** `400`, since below two characters every row matches and above two hundred a trigram scan stops being cheap - `inst-sd-search-1`
2. [ ] - `p2` - Authorize `read` on the value resource once for the request; its constraints are the secure scope of the declarations query, so a setting the caller may not read is absent from the results and from the count - `inst-sd-search-2`
3. [ ] - `p2` - Confirm the target is the caller's own tenant or a descendant that is not standalone; **IF** not → **RETURN** `403` - `inst-sd-search-3`
4. [ ] - `p2` - Bound the override corpus to the target and its non-standalone descendants, obtained from the tenant resolver: an override the caller could not read is never matched - `inst-sd-search-4`
5. [ ] - `p2` - Decide the classification corpus once, before any match: `public`, and `pii` only for a caller holding `read_unmasked` on the value resource; `secret` never - `inst-sd-search-5`
6. [ ] - `p2` - Bind the pagination cursor to the query text, the target and the corpus, so a cursor minted for one search is refused for another - `inst-sd-search-6`
7. [ ] - `p2` - DB: SELECT a page of active declarations, ordered by key, that match on key, description, the name of their category, their Schema Default within the corpus, or an override set at one of the bounded tenants within the corpus; domain visibility and the secure scope apply in the same query - `inst-sd-search-7`
8. [ ] - `p2` - DB: SELECT the overrides of the page's declarations at the bounded tenants whose text projection matches, within the corpus and never a secret row - `inst-sd-search-8`
9. [ ] - `p2` - Attribute each declaration-level match to the first field that matched — key, description, category name, Schema Default — and emit one hit per matching override naming the tenant and scope where it is set - `inst-sd-search-9`
10. [ ] - `p2` - Exclude every hit whose declaration is `hidden` for the caller, silently, exactly as browse excludes it - `inst-sd-search-10`
11. [ ] - `p2` - **RETURN** `200` with the flat list — each hit carrying its category, its matched field, its declaration's `mode` as a tag, and, where a value matched, that value masked by classification — and the page cursors - `inst-sd-search-11`

## 3. Processes / Business Logic (CDSL)

### Query Validation and Pattern

- [ ] `p2` - **ID**: `cpt-cf-settings-service-algo-search-discoverability-needle`

**Input**: The raw `q` parameter

**Output**: A validated needle, its `LIKE` pattern, and the dialect's operator

**Steps**:
1. [ ] - `p2` - Trim `q`; **IF** fewer than two characters or more than two hundred remain → refuse on field `q` - `inst-sd-needle-1`
2. [ ] - `p2` - Build the pattern `%needle%` with `%`, `_` and `\` escaped by `\`, and declare `ESCAPE '\'` on every predicate that uses it, because SQLite has no default escape character - `inst-sd-needle-2`
3. [ ] - `p2` - Choose the operator by dialect: `ILIKE` on PostgreSQL over exactly the expressions the trigram indexes are built on, `LIKE` on SQLite where the match is a scan; the two branches differ in spelling only - `inst-sd-needle-3`

### Corpus by Classification

- [ ] `p2` - **ID**: `cpt-cf-settings-service-algo-search-discoverability-corpus`

**Input**: The caller's `read_unmasked` decision, the bounded tenant set

**Output**: The predicates that admit a stored value to matching

**Steps**:
1. [ ] - `p2` - Exclude every `secret` row by predicate — `secret_ref IS NULL` and a classification in the corpus — so a secret cannot be discovered through match existence, count or timing - `inst-sd-corpus-1`
2. [ ] - `p2` - Admit `pii` rows only when the caller holds `read_unmasked`; otherwise the corpus is `public` alone and PII content is unreachable through a match - `inst-sd-corpus-2`
3. [ ] - `p2` - Exclude a JSON `null` Schema Default from the default corpus, since its text projection is the literal `null` and would match that word on every such setting - `inst-sd-corpus-3`
4. [ ] - `p2` - State the classification predicate identically in the page query and in the override query, so on PostgreSQL each is served by the matching half of the split index pair and correctness rests on the predicate, not on the plan - `inst-sd-corpus-4`

### Matched-Field Attribution

- [ ] `p2` - **ID**: `cpt-cf-settings-service-algo-search-discoverability-attribution`

**Input**: A declaration the database returned, its category, the matching override rows

**Output**: The hits for that declaration

**Steps**:
1. [ ] - `p2` - Test the fields in the order a client is told about them — key, description, category name, Schema Default — and attribute the declaration-level hit to the first that contains the needle case-insensitively; a declaration on the page only because an override matched yields no declaration-level hit - `inst-sd-attr-1`
2. [ ] - `p2` - Match a JSON value by its text projection: a string as itself, anything else as its JSON text — the same projection the database indexes - `inst-sd-attr-2`
3. [ ] - `p2` - **IF** the database matched a declaration but no field and no override names the match in Rust — whitespace inside a JSON projection, or a case fold the two engines disagree on — attribute it to the Schema Default when that is in the corpus, else to the key, so a row the database returned is never silently dropped - `inst-sd-attr-3`

## 4. States (CDSL)

No stateful entity: search reads and stores nothing.

## 5. Definitions of Done

### Search Surface

- [ ] `p2` - **ID**: `cpt-cf-settings-service-dod-search-discoverability-surface`

`GET /settings-service/v1/search` **MUST** be served authenticated, take `q`, `tenant`, `limit` and `cursor`, refuse OData options, and answer a cursor-paginated flat list of hits under the same authorization, target, visibility and `hidden` rules as browsing.

**Implements**:
- `cpt-cf-settings-service-flow-search-discoverability-search`

**Touches**:
- Entities: SettingDeclaration, Category, SettingValue

### Classification-Aware Corpus

- [ ] `p2` - **ID**: `cpt-cf-settings-service-dod-search-discoverability-corpus`

A `secret` value **MUST NOT** be matched under any query; a `pii` value **MUST** be matched only for a caller holding `read_unmasked`; an override **MUST** be matched only at the target and its non-standalone descendants; a JSON `null` default **MUST NOT** match.

**Implements**:
- `cpt-cf-settings-service-algo-search-discoverability-corpus`

**Touches**:
- Entities: SettingDeclaration, SettingValue

### Hits

- [ ] `p2` - **ID**: `cpt-cf-settings-service-dod-search-discoverability-hits`

A hit **MUST** carry the setting key, declaration id, leaf slug, description, its category as `{id, key, name}`, the matched field from `key | description | category_name | default_value | value`, and the declaration's `mode`; an override hit **MUST** name `scope` and `tenant_id`; a value **MUST** be present only on a `value` or `default_value` hit and masked by classification as a read would mask it.

**Implements**:
- `cpt-cf-settings-service-algo-search-discoverability-attribution`

**Touches**:
- Entities: SettingDeclaration, Category, SettingValue

### Dialect

- [ ] `p2` - **ID**: `cpt-cf-settings-service-dod-search-discoverability-dialect`

On PostgreSQL the predicates **MUST** use `ILIKE` over the exact indexed expressions; on SQLite they **MUST** use `LIKE` with the same escaping; the query **MUST** be one statement per page plus one per override set, whatever the dialect.

**Implements**:
- `cpt-cf-settings-service-algo-search-discoverability-needle`

**Touches**:
- Entities: SettingDeclaration, SettingValue

## 6. Acceptance Criteria

- [ ] A needle matching a setting key yields one declaration-level hit with `matched_field: key` and no scope
- [ ] A needle matching only a description yields `matched_field: description`
- [ ] A needle matching only a category's name yields `matched_field: category_name` for every setting in it, with the category as breadcrumb
- [ ] A needle matching a `public` Schema Default yields `matched_field: default_value` with the default as `value` and no scope
- [ ] A setting whose Schema Default is JSON `null` is not matched by the needle `null`
- [ ] A needle matching an override set at a descendant yields a `value` hit naming that tenant and its scope path
- [ ] An override set at a standalone descendant is not matched from above
- [ ] An override set outside the target's subtree is not matched
- [ ] A `secret` value is never matched, whether the needle is its stored reference or any text
- [ ] A `pii` default or override is matched only for a caller holding `read_unmasked`; without it neither a hit nor a count reveals it
- [ ] `%`, `_` and `\` in the needle match literally
- [ ] A retired declaration is not matched
- [ ] A declaration `hidden` for the caller is absent from the results
- [ ] `q` of one character, or of two hundred and one, is refused `400` on field `q`
- [ ] `$filter`, `$orderby` or `$select` on the resource is refused `400`
- [ ] A page holds at most `limit` settings, ordered by key, and the cursor continues from the last one; a cursor from a different needle, target or corpus is refused
- [ ] Every hit carries its declaration's `mode`, and no hit is withheld by it
