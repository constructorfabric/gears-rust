# UPSTREAM_REQS — Todo App

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting Gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Sync Service](#21-sync-service)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)
  - [Sync Service Sources](#sync-service-sources)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Todo App is a task management gear. This document consolidates requirements from gears that depend on Todo App to expose task data and change tracking for synchronization across devices.

### 1.2 Requesting Gears

| Gear | Why it needs this gear |
|--------|-------------------------|
| sync-service | Needs Todo App to track task modification timestamps and expose a changes feed for incremental cross-device synchronization |

## 2. Requirements

### 2.1 Sync Service

#### Track Modification Timestamps

- [ ] `p1` - **ID**: `cpt-examples-todo-app-upreq-modification-timestamps`

The gear **MUST** record a last-modified timestamp on every task change (create, update, complete, delete) so that the sync service can request only changes since a given point in time.

- **Rationale**: Without per-task modification timestamps, the sync service must transfer the entire task set on every sync cycle, which does not scale and wastes bandwidth.
- **Source**: `gears/sync-service`

#### Expose Changes Feed

- [ ] `p1` - **ID**: `cpt-examples-todo-app-upreq-changes-feed`

The gear **MUST** expose an ordered feed of task changes (created, updated, completed, deleted) filterable by timestamp, so the sync service can perform incremental synchronization.

- **Rationale**: Sync service needs a pull-based mechanism to discover what changed since the last sync checkpoint; polling the full task list is insufficient for real-time sync targets (<5s).
- **Source**: `gears/sync-service`

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| p1 (critical) | `cpt-examples-todo-app-upreq-modification-timestamps`, `cpt-examples-todo-app-upreq-changes-feed` |

## 4. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **Design**: [DESIGN.md](./DESIGN.md)

### Sync Service Sources

<!-- Source IDs will be added when sync-service gear is created -->
