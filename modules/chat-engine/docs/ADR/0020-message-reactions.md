Created:  2026-02-11 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0020: Message Reactions with Simple Like/Dislike


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Technical Design](#technical-design)
  - [Database Schema](#database-schema)
  - [HTTP API](#http-api)
  - [Webhook Event](#webhook-event)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Separate reaction table with UPSERT and fire-and-forget webhook](#option-1-separate-reaction-table-with-upsert-and-fire-and-forget-webhook)
  - [Option 2: Reaction counts in messages table](#option-2-reaction-counts-in-messages-table)
  - [Option 3: Rich reaction system with emoji](#option-3-rich-reaction-system-with-emoji)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-11

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-message-reactions`

## Context and Problem Statement

Chat Engine needs to support user feedback on individual messages through reactions. Users should be able to express simple sentiment (like/dislike) on any message, change their reaction, or remove it entirely. How should reactions be modeled to support this while maintaining data integrity, idempotency, and architectural consistency with existing webhook patterns?

## Decision Drivers

* Users need ability to react to individual messages with simple like/dislike
* One reaction per user per message (not multiple reactions)
* Users can change their reaction or remove it entirely
* Reactions should not modify message immutability principle
* Backend systems need notification of reaction events for analytics
* Reaction storage must be efficient and support concurrent operations
* Solution must integrate cleanly with existing HTTP and webhook APIs
* Failures in backend notification should not block user reactions

## Considered Options

* **Option 1: Separate reaction table with UPSERT and fire-and-forget webhook** - Dedicated message_reactions table with composite PK, special "none" value for removal, fire-and-forget webhook notification
* **Option 2: Reaction counts in messages table** - Store like_count/dislike_count directly in messages table, update via transactions
* **Option 3: Rich reaction system with emoji** - Support arbitrary emoji reactions with multiple reactions per user per message

## Decision Outcome

Chosen option: "Separate reaction table with UPSERT and fire-and-forget webhook", because it maintains message immutability, provides clean separation of concerns, supports efficient UPSERT semantics with composite primary key, enables atomic operations without conflicts, and follows established fire-and-forget webhook pattern for non-critical notifications.

### Consequences

* Good, because message immutability principle is preserved (reactions stored separately)
* Good, because composite PK (message_id, user_id) enforces business rule at database level
* Good, because UPSERT semantics make API idempotent and safe for retries
* Good, because CASCADE DELETE automatically cleans up reactions when messages deleted
* Good, because fire-and-forget webhook matches pattern of message.aborted and session.deleted
* Good, because backend analytics can process reaction events without blocking clients
* Bad, because querying reaction counts requires aggregation (not pre-computed)
* Bad, because "none" special value creates tristate enum (like/dislike/none)
* Bad, because no reaction history preserved (only current state stored)

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

## Technical Design

### Database Schema

Table `message_reactions` with composite primary key (message_id, user_id). Columns: message_id (UUID), user_id (VARCHAR), reaction_type (VARCHAR with CHECK constraint for 'like'/'dislike'), created_at, updated_at timestamps. Foreign key to messages table with CASCADE DELETE. Index on message_id for efficient lookups.

### HTTP API

**Endpoint**: `POST /messages/{id}/reaction`

**Request**: `{reaction_type: "like" | "dislike" | "none"}`

**Response**: `{message_id: UUID, reaction_type: string, applied: boolean}`

**Semantics**:
- `"like"` or `"dislike"`: UPSERT reaction (insert or update if exists)
- `"none"`: DELETE existing reaction (returns applied: false)

### Webhook Event

**Event Type**: `message.reaction`

**Payload**: MessageReactionEvent with:
- event, session_id, message_id, user_id, reaction_type
- `previous_reaction_type`: null (first reaction) | "like" | "dislike" (changed) | "none" (removed)
- timestamp

**Pattern**: Fire-and-forget (required: false, no retry, no circuit breaker)

**Flow**:
1. Client sends reaction → Database UPSERT/DELETE → Client receives 200 OK
2. Webhook sent asynchronously → Backend processes event → Failure logged but doesn't affect client

## Pros and Cons of the Options

### Option 1: Separate reaction table with UPSERT and fire-and-forget webhook

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 2: Reaction counts in messages table

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 3: Rich reaction system with emoji

See "Considered Options" and "Consequences" above for trade-off analysis.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` - Submits reactions via HTTP POST, receives immediate confirmation
* `cpt-cf-chat-engine-actor-backend-plugin` - Receives reaction events for analytics and side effects

**Requirements**:
* cpt-cf-chat-engine-fr-message-reactions - Users can like/dislike messages
* cpt-cf-chat-engine-fr-reaction-change - Users can change or remove their reaction
* cpt-cf-chat-engine-nfr-reaction-idempotency - Multiple identical requests produce same result
* `cpt-cf-chat-engine-nfr-data-integrity` - Composite PK enforces one reaction per user per message

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-message-reaction` - Reaction entity with composite key
* cpt-cf-chat-engine-api-http-reaction - HTTP endpoint POST /messages/{id}/reaction
* cpt-cf-chat-engine-webhook-message-reaction - Webhook event message.reaction
* `cpt-cf-chat-engine-principle-immutable-tree` - Reactions don't modify messages
* Sequence diagrams: S14 (Add Message Reaction), S15 (Remove Message with Reactions)

**Related ADRs**:
* ADR-0001 (Message Tree with Immutable Parents) - Reactions preserve message immutability
* ADR-0004 (Zero Business Logic in Routing) - Chat Engine stores reactions, backend validates via webhook
* ADR-0009 (Stateless Service Design) - Reaction state in database, any instance can handle requests
