Created:  2026-02-04 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# ADR-0005: External File Storage for File Attachments


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
  - [UUID vs URL Approach](#uuid-vs-url-approach)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Separate File Storage service with UUID identifiers](#option-1-separate-file-storage-service-with-uuid-identifiers)
  - [Option 2: Separate File Storage service with URL identifiers](#option-2-separate-file-storage-service-with-url-identifiers)
  - [Option 3: Database BLOB storage](#option-3-database-blob-storage)
  - [Option 4: Chat Engine file service](#option-4-chat-engine-file-service)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-04

**Status**: accepted

**ID**: `cpt-cf-chat-engine-adr-file-handling`

## Context and Problem Statement

Users need to attach files to messages (images, documents, code files) for context-aware AI responses. Where should file content be stored, and how should Chat Engine handle file data as messages flow through the system?

## Decision Drivers

* File sizes can be large (up to 10MB per file, 50MB per message)
* Chat Engine focuses on message routing and tree management, not file storage
* Storage costs should be optimized (file storage cheaper than database)
* Webhook backends need direct file access for processing
* Clients should upload files quickly without Chat Engine bottleneck
* Infrastructure complexity should be minimized
* File durability and availability requirements match file storage capabilities

## Considered Options

* **Option 1: Separate File Storage service with UUID identifiers** - Clients upload to File Storage service, messages contain stable file UUIDs
* **Option 2: Separate File Storage service with URL identifiers** - Clients upload to File Storage service, messages contain file URLs
* **Option 3: Database BLOB storage** - File content stored in PostgreSQL as bytea/BLOB columns
* **Option 4: Chat Engine file service** - Chat Engine provides upload endpoint, stores files on disk/storage

## Decision Outcome

Chosen option: "Separate File Storage service with UUID identifiers", because it eliminates file handling from Chat Engine critical path, leverages optimized file storage infrastructure, enables direct client uploads reducing latency, allows webhook backends direct file access, minimizes Chat Engine storage and bandwidth costs, and provides stable identifiers that enable centralized access control and transparent storage migration.

### Consequences

* Good, because clients upload to File Storage service bypassing Chat Engine
* Good, because Chat Engine only stores small file UUIDs (not large file content or expiring URLs)
* Good, because File Storage service provides file management with durability, availability, and CDN integration
* Good, because webhook backends can download files directly from File Storage using stable UUIDs
* Good, because File Storage service manages storage optimization
* Good, because Chat Engine infrastructure remains simple (no file storage management)
* Good, because UUIDs are stable identifiers that never expire
* Good, because centralized access control through File Storage Service API
* Good, because transparent storage migration without updating message records
* Bad, because requires external file storage service deployment and configuration
* Bad, because webhook backends must integrate with File Storage Service API
* Bad, because clients need additional API call to File Storage when displaying files
* Bad, because file lifecycle management is separate from session lifecycle
* Bad, because clients must implement upload-then-message-send flow

### Confirmation

Confirmed via design review and alignment with DESIGN.md implementation.

### UUID vs URL Approach

**Decision**: Store file UUIDs instead of URLs in message records.

**Rationale**:
- UUIDs are stable and do not expire (signed URLs expire)
- Enables centralized access control through File Storage Service
- Allows transparent storage migration without updating messages
- Clear separation: UUID = identifier, URL = access token (generated on-demand)
- Reduces security risk of URL leakage in logs or external systems

**Trade-offs**:
- Webhook backends require File Storage Service integration
- Additional API call needed when clients display files
- File Storage Service must provide UUID-based file retrieval API
- Increased operational dependency on File Storage availability

**Data Flow**:
- Chat Engine stores file UUIDs (stable identifiers) in message records
- Clients upload directly to file storage, receive UUIDs
- Webhook Backends fetch files from File Storage Service API using UUIDs
- Clients request temporary signed URLs from File Storage when displaying files

## Pros and Cons of the Options

### Option 1: Separate File Storage service with UUID identifiers

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 2: Separate File Storage service with URL identifiers

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 3: Database BLOB storage

See "Considered Options" and "Consequences" above for trade-off analysis.

### Option 4: Chat Engine file service

See "Considered Options" and "Consequences" above for trade-off analysis.

## Related Design Elements

**Actors**:
* `cpt-cf-chat-engine-actor-file-storage` - Separate File Storage service managing file uploads, UUID-based retrieval, and signed URL generation
* `cpt-cf-chat-engine-actor-client` - Uploads files to storage, receives UUIDs, includes UUIDs in messages
* `cpt-cf-chat-engine-actor-backend-plugin` - Fetches files from File Storage Service using UUIDs

**Requirements**:
* `cpt-cf-chat-engine-fr-attach-files` - Messages support file_ids array field (UUIDs)
* `cpt-cf-chat-engine-nfr-file-size` - Limits enforced by storage service, not Chat Engine
* `cpt-cf-chat-engine-nfr-response-time` - File handling off critical path

**Design Elements**:
* `cpt-cf-chat-engine-design-entity-message` - Contains file_ids (UUID array) not file content or URLs
* `cpt-cf-chat-engine-constraint-external-storage` - Design constraint mandating separate File Storage service
* `cpt-cf-chat-engine-design-context-file-storage` - Implementation details for UUID-based file access

**Related ADRs**:
* ADR-0009 (Stateless Horizontal Scaling with Database State) - Database not used for file content storage
