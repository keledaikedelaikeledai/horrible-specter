# Serangkai

Serangkai is a terminal-native AI coding workspace for using multiple coding
harnesses within one continuous, application-owned conversation.

## Run

```bash
cargo run
cargo run -- --help
cargo run -- /path/to/project
```

The first milestone launches a Ratatui interface, persists conversations and
messages to `.serangkai/serangkai.db`, shows read-only Git status, and clearly
indicates that no provider is configured. Provider execution is intentionally
not simulated.

## Controls

- `j` / `k`: navigate and scroll
- `h` / `l`: change focused panel
- `i`: compose a message; `Enter` saves it locally; `Shift+Enter` adds a line;
  `Esc` leaves compose mode
- `Tab` / `Shift+Tab`: cycle panels
- `Ctrl+p`: command palette
- `Ctrl+a`: provider selector
- `Ctrl+b`: toggle project sidebar
- `Ctrl+d`: toggle Git sidebar
- `?`: keyboard help
- `q`: quit in normal mode

## Architecture

- `src/app.rs`: application state, event loop, and persistence coordination
- `src/input.rs`: keyboard modes, navigation, and command actions
- `src/ui.rs`: Ratatui rendering only
- `src/provider.rs`: provider-neutral streaming and tool-event contract
- `src/persistence.rs`: versioned SQLite migrations and atomic event recording
- `src/handoff.rs`: application-owned handoff context contracts
- `src/workspace.rs`: read-only Git/workspace integration and refresh data
- `src/terminal.rs`: terminal lifecycle and panic restoration

See [ARCHITECTURE.md](ARCHITECTURE.md) for runtime boundaries, persistence
semantics, provider contracts, and workspace safety rules.
