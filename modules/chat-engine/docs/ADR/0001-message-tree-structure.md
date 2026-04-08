Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0001: Message Tree with Immutable Parents


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Immutable tree with parent_message_id](#option-1-immutable-tree-with-parentmessageid)
  - [Option 2: Mutable linked list](#option-2-mutable-linked-list)
  - [Option 3: Graph structure with edge table](#option-3-graph-structure-with-edge-table)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-message-tree-structure`

## Context and Problem Statement

Chat Engine needs to support conversation branching, message regeneration, and variant exploration while maintaining referential integrity and enabling safe concurrent message creation. How should messages be structured to enable these capabilities without introducing data inconsistencies or race conditions?

## Decision Drivers

* Support for conversation branching from any historical message
* Ability to regenerate assistant responses creating variants
* Referential integrity must be enforced at database level
* Safe concurrent message creation across multiple sessions
* Natural representation of conversation alternatives
* Support for history navigation and variant exploration
* Immutable conversation history for audit and debugging

## Considered Options

* **Option 1: Immutable tree with parent_message_id** - Messages form tree structure where each message references immutable parent via parent_message_id
* **Option 2: Mutable linked list** - Messages form doubly-linked list with next/previous pointers that can be updated
* **Option 3: Graph structure with edge table** - Separate table stores relationships between messages allowing arbitrary connections

## Decision Outcome

Chosen option: "Immutable tree with parent_message_id", because it provides natural representation of conversation structure, enables database-enforced referential integrity via foreign keys, supports safe concurrent writes without conflicts, and makes branching explicit through shared parent relationships.

### Consequences

* Good, because database foreign key constraints enforce tree integrity automatically
* Good, because immutability prevents accidental corruption of conversation history
* Good, because concurrent message creation is safe (different parents = no conflicts)
* Good, because variants are naturally represented as siblings (same parent)
* Good, because tree structure maps directly to conversation branching semantics
* Bad, because traversal queries require recursive CTEs for deep trees
* Bad, because calculating active path requires following is_active flags
* Bad, because re-parenting messages is impossible (by design, ensuring immutability)

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

## Pros and Cons of the Options

### Option 1: Immutable tree with parent_message_id

* Good, because database foreign key constraints enforce tree integrity automatically
* Good, because immutability prevents accidental corruption of conversation history
* Good, because concurrent message creation is safe (different parents = no conflicts)
* Good, because variants are naturally represented as siblings sharing the same parent
* Bad, because traversal queries require recursive CTEs for deep trees
* Bad, because calculating active path requires following is_active flags through the tree
* Bad, because re-parenting messages is impossible (by design, ensuring immutability)

### Option 2: Mutable linked list

* Good, because traversal is simple (follow next/previous pointers without recursive queries)
* Good, because active path is implicit in the linked list order
* Bad, because mutable pointers create race conditions under concurrent writes
* Bad, because branching requires duplicating and relinking portions of the list
* Bad, because referential integrity is fragile (pointer updates can break the chain)

### Option 3: Graph structure with edge table

* Good, because arbitrary relationships between messages are possible (maximum flexibility)
* Good, because re-parenting and complex topologies are supported natively
* Bad, because additional join through edge table adds query complexity and latency
* Bad, because no inherent constraint prevents cycles or inconsistent graph structures
* Bad, because conversation branching semantics are not naturally represented (overengineered for tree-shaped data)

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-client` - Navigates message tree and requests branching operations
* `cpt-cf-chat-engine-actor-backend-plugin` - Receives truncated history based on tree traversal

**Requirements**:
* `cpt-cf-chat-engine-fr-recreate-response` - Variants created as siblings with same parent_message_id
* `cpt-cf-chat-engine-fr-branch-message` - New messages reference historical message as parent
* `cpt-cf-chat-engine-fr-navigate-variants` - Query siblings by parent_message_id for variant navigation
* `cpt-cf-chat-engine-nfr-data-integrity` - Database constraints enforce tree structure integrity

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-message` - Core entity implementing tree structure
* `cpt-cf-chat-engine-principle-immutable-tree` - Design principle mandating immutability
* `cpt-cf-chat-engine-design-context-tree-traversal` - Implementation details for traversal queries

**Related ADRs**:
* ADR-0011 (Message Variants with Index and Active Flag) - Depends on this tree structure
* ADR-0013 (Recreation Creates Variants, Branching Creates Children) - Uses parent_message_id to create variants
* ADR-0014 (Conversation Branching from Any Historical Message) - Leverages tree structure for branching
