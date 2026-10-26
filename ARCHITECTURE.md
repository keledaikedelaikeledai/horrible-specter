# Serangkai architecture

Serangkai keeps application-owned conversation history separate from provider
native state. Providers will eventually stream observable events into the
application; they do not own the conversation or the workspace lifecycle.

## Runtime flow

```text
terminal input -> input mapping -> App state -> Ratatui renderer
                       |                 |
                       |                 +-> SQLite event recorder
                       |
                       +-> provider adapter (future)

Tokio event loop -> bounded workspace refresh requests
                 -> one refresh worker
                 -> spawn_blocking(git)
                 -> latest WorkspaceStatus
```

The TUI does not execute provider-specific logic. `src/app.rs` coordinates
state changes, `src/input.rs` maps keyboard events, and `src/ui.rs` renders the
state. Git commands run in a single coalescing worker so a slow refresh cannot
block input or overlap another refresh.

## Persistence

SQLite lives at `.serangkai/serangkai.db` for the selected project. Schema
migrations are tracked in `schema_migrations`.

- `projects` associates conversations with local directories.
- `conversations` stores titles and lifecycle metadata.
- `events` is the append-only observable record for messages, tool activity,
  provider notices, and terminal outcomes.
- `provider_sessions` stores native session identifiers separately from the
  application conversation.
- `handoff_checkpoints` stores future transfer context.

Event insertion uses an immediate transaction. Sequence allocation, event
insertion, and conversation `updated_at`/title updates commit together. A
provider-supplied event key is unique within a conversation, allowing replayed
stream events to be ingested idempotently.

## Provider contract

`src/provider.rs` defines the boundary for future adapters:

- `SessionRequest` supports new and native-session resume flows.
- `ApplicationContext` carries application-owned history independently of
  native provider state.
- `ProviderEvent` has a stable event identity and represents message deltas,
  tool output, completion, interruption, notices, and failures.
- `ProviderError::UsageLimit` is distinct from authentication, transport,
  protocol, interruption, and generic failures.
- `OperationStatus::Partial` and `OperationStatus::Uncertain` preserve the
  fact that a tool or provider operation may not have reached a clean end.

No provider integration is included in this milestone.

## Workspace safety

Git integration is read-only. It reports porcelain status, staged diff,
unstaged diff, branch information, and a bounded preview. Serangkai does not
stage, reset, checkout, clean, write, or discard workspace files.

## Handoff boundary

`src/handoff.rs` describes a checkpoint containing application history, an
optional provider-native state blob, and a workspace snapshot. Native state is
supplemental: a future adapter must still be able to continue from the
application-owned history if native resumption is unavailable.
