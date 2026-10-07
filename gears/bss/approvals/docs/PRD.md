Created:  2026-10-01 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

# PRD — BSS Approvals Inbox

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [One inbox](#one-inbox)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [No store of its own](#no-store-of-its-own)
- [7. Public Library Interfaces](#7-public-library-interfaces)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

The approvals inbox is one read-and-route facade over the approval units of every BSS gear. The pricing screen and its navigation badge read one paged list, one count and one card, and cast one vote, instead of calling each gear and merging the answers themselves.

### 1.2 Background / Problem Statement

Pricing and products each keep their own approval units and decide them inside their own transactions. A shared table would split that decision from the domain write. The screen currently merges two doors. The inbox leaves the units where they are and merges what the caller can already read.

### 1.3 Goals (Business Outcomes)

- One list, one count, one card and one vote for every BSS approval unit the caller can read.
- A gear that the caller cannot read is visible as forbidden and contributes nothing.
- A vote through the inbox is the same answer as the owning gear's own vote door.

### 1.4 Glossary

- **Source.** A BSS gear that registers its approval units for the inbox.
- **Narrowing.** The state, kind, referenced aggregate and book the list and the count share.
- **Readable source.** A configured source that answered the caller, rather than refusing the grant.

## 2. Actors

### 2.1 Human Actors

An approver uses the inbox after the gateway has authenticated the call. The owning gear judges the grant.

### 2.2 System Actors

A BSS gear registers a source of its own units under its stable name. Pricing and products are the sources in this version. A later gear joins by implementing the same port and adding its name to the configuration.

## 3. Operational Concept & Environment

The inbox runs in the same process as the gears it reads. It has no database of its own. When its configuration object is absent, it does not serve. When the object is present, `sources` names the gears it asks, in that order.

## 4. Scope

### 4.1 In Scope

- One list, ordered by submission time and then by unit id, paged with a cursor.
- One count of that same narrowing, summed over the readable sources.
- One card, resolved to the single gear that holds the unit.
- Approve, reject and withdraw, forwarded to that gear's own vote door without rewriting the answer.

### 4.2 Out of Scope

- Removing the per-gear doors.
- An inbox table fed by events.
- Notifications.
- Subscription and billing units, until those gears implement the source port.

## 5. Functional Requirements

### One inbox

- [ ] `p1` - **ID**: `cpt-cf-bss-approvals-fr-one-inbox`

The inbox MUST serve one paged list, one count and one card of approval units, and MUST forward approve, reject and withdraw to the gear that holds the unit.

The configured source names are non-blank and unique. A duplicate or a blank name fails the boot. A source's counts name only the closed kind set: a kind that is absent is zero, and a kind field outside the set does not decode.

The list orders by `submitted_at` as an instant and then by the unit id, in the same direction, newest first when the caller omits the order. A continuation uses the cursor's order. A gear the caller may not read is omitted and named forbidden. A gear that does not answer is omitted and named unavailable; the list and the counts still answer the gears that did, and fail only when every gear is down or every gear forbids the caller (AP-D-5). A continuation does not resume a gear the cursor recorded as down. The count includes only the readable sources.

The card asks every configured source. One hit wins. Two hits are an error naming both. A vote is that gear's own answer: status, headers and body, unchanged. The caller sends `Idempotency-Key` on approve, reject and withdraw; the inbox never mints one (AP-D-6).

## 6. Non-Functional Requirements

### No store of its own

- [ ] `p1` - **ID**: `cpt-cf-bss-approvals-nfr-no-store`

The inbox MUST NOT own a database table and MUST NOT open a database connection. A page's work, beyond the calls to the configured sources, is the in-memory merge of the pages those sources returned.

## 7. Public Library Interfaces

The source port is the SDK crate `cf-gears-bss-approvals-sdk`. A gear registers `ApprovalSourceV1` in ClientHub under its own scope. The port's methods are `page`, `counts`, `get` and one `vote`.

## 8. Use Cases

An approver opens the queue. The inbox asks every configured source, drops the ones that refuse the caller, merges the rest by submission time and unit id, and returns one page plus the count of that narrowing.

The approver opens one unit and votes. The inbox finds the single source that holds the id and returns that source's vote answer unchanged.

## 9. Acceptance Criteria

- A walk of a mixed set of units, in either order, returns every unit once.
- A forbidden source is named and contributes nothing. A source that does not answer is named unavailable and contributes nothing; the read fails only when every source is down.
- The card distinguishes one holder, two holders, a refusal, an unavailable source and a miss.
- A vote forwarded through the inbox is byte-for-byte the owning door's answer.
- The gear starts without a database, and it does not serve when its configuration object is absent.

## 10. Dependencies

- The gateway authenticates the caller.
- Each source gear authorizes and stores its own units. Pricing and products are the first two. Their doors stay the doors they already have.
- ClientHub carries one scoped source client per gear name.

## 11. Assumptions

- The caller compares submission times as instants, and unit ids in lower-case hex order. The inbox uses that same order.
- A source applies the keyset with its own list door. The inbox does not issue a second predicate.
- Stored instants are whole microseconds, so an instant compare across gears is exact.

## 12. Risks

- A source that is down fails the whole read, so the caller does not see a partial queue that looks complete.
- Two gears holding one id is a 500. That collision is a data fault, not a merge rule the caller can page through.
