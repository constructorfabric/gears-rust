# DECOMPOSITION — Todo App

- [ ] `p1` - **ID**: `cpt-examples-todo-app-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Task CRUD ⏳ HIGH](#21-task-crud--high)
  - [2.2 Notifications ⏳ MEDIUM](#22-notifications--medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

Todo App design is decomposed into features organized around core task management capabilities. The decomposition follows a dependency order where foundational CRUD operations enable higher-level features like notifications and reporting.

**Decomposition Strategy**:
- Features grouped by functional cohesion (related capabilities together)
- Dependencies minimize coupling between features
- Each feature covers specific components and sequences from DESIGN
- 100% coverage of all DESIGN elements verified

## 2. Entries

### 2.1 [Task CRUD](feature-task-crud/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-ex-task-flow-feature-task-crud`

- **Purpose**: Enable users to create, view, edit, and delete tasks with full lifecycle management.

- **Depends On**: None

- **Scope**:
  - Task creation with title, description, priority, due date
  - Task assignment to team members
  - Status transitions (BACKLOG → IN_PROGRESS → DONE)
  - Task deletion with soft-delete

- **Out of scope**:
  - Recurring tasks
  - Task templates

- **Requirements Covered**:

  - [ ] `p1` - `cpt-ex-task-flow-fr-task-crud`
  - [ ] `p2` - `cpt-ex-task-flow-nfr-performance-reliability`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-ex-task-flow-principle-realtime-first`
  - [ ] `p2` - `cpt-ex-task-flow-principle-simplicity-over-features`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-ex-task-flow-constraint-supported-platforms`

- **Domain Model Entities**:
  - Task
  - User

- **Design Components**:

  - [ ] `p1` - `cpt-ex-task-flow-component-react-spa`
  - [ ] `p1` - `cpt-ex-task-flow-component-api-server`
  - [ ] `p1` - `cpt-ex-task-flow-component-postgresql`
  - [ ] `p2` - `cpt-ex-task-flow-component-redis-pubsub`

- **API**:
  - POST /api/tasks
  - GET /api/tasks
  - PUT /api/tasks/{id}
  - DELETE /api/tasks/{id}

- **Sequences**:

  - [ ] `p1` - `cpt-ex-task-flow-seq-task-creation`

- **Data**:

  - [ ] `p1` - `cpt-ex-task-flow-dbtable-tasks`

### 2.2 [Notifications](feature-notifications/) ⏳ MEDIUM

- [ ] `p2` - **ID**: `cpt-ex-task-flow-feature-notifications`

- **Purpose**: Notify users about task assignments, due dates, and status changes.

- **Depends On**: `cpt-ex-task-flow-feature-task-crud`

- **Scope**:
  - Push notifications for task assignments
  - Email alerts for overdue tasks
  - In-app notification center

- **Out of scope**:
  - SMS notifications
  - Custom notification templates

- **Requirements Covered**:

  - [ ] `p2` - `cpt-ex-task-flow-fr-notifications`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-ex-task-flow-principle-realtime-first`
  - [ ] `p2` - `cpt-ex-task-flow-principle-mobile-first`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-ex-task-flow-constraint-supported-platforms`

- **Domain Model Entities**:
  - Task
  - User
  - Notification

- **Design Components**:

  - [ ] `p1` - `cpt-ex-task-flow-component-react-spa`
  - [ ] `p1` - `cpt-ex-task-flow-component-api-server`
  - [ ] `p2` - `cpt-ex-task-flow-component-redis-pubsub`

- **API**:
  - POST /api/notifications
  - GET /api/notifications
  - PUT /api/notifications/{id}/read

- **Sequences**:

  - [ ] `p2` - `cpt-ex-task-flow-seq-notification-delivery`

- **Data**:

  - [ ] `p2` - `cpt-ex-task-flow-dbtable-notifications`

## 3. Feature Dependencies

```text
cpt-examples-todo-app-feature-core
    ↓
    ├─→ cpt-examples-todo-app-feature-logic
    └─→ cpt-examples-todo-app-feature-sync
```

**Dependency Rationale**:

- `cpt-examples-todo-app-feature-logic` requires `cpt-examples-todo-app-feature-core`: Cannot organize/filter tasks that don't exist
- `cpt-examples-todo-app-feature-sync` requires `cpt-examples-todo-app-feature-core`: Cannot sync tasks that haven't been created
- `cpt-examples-todo-app-feature-logic` and `cpt-examples-todo-app-feature-sync` are independent of each other and can be developed in parallel
