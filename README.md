# Serangkai

Serangkai is a terminal-native AI coding workspace for using multiple coding
harnesses within one continuous, application-owned conversation.

## Run

```bash
cargo run
cargo run -- --help
cargo run -- /path/to/project
```

Serangkai connects to an existing OpenCode server at
`http://127.0.0.1:4096` by default. Start one from the project directory with:

```bash
opencode serve --port 4096
```

Set `OPENCODE_URL` when the server uses another address. Authentication remains
owned by OpenCode; Serangkai also honors `OPENCODE_SERVER_USERNAME` and
`OPENCODE_SERVER_PASSWORD` when connecting to a protected server.

## Controls

- `j` / `k`: navigate and scroll
- `h` / `l`: change focused panel
- `i`: compose a message; `Enter` submits it to OpenCode; `Shift+Enter` adds a
  line; `Esc` leaves compose mode
- `Tab` / `Shift+Tab`: cycle panels
- `Ctrl+p`: command palette
- `Ctrl+a`: select an available OpenCode model
- `Ctrl+b`: toggle project sidebar
- `Ctrl+d`: toggle Git sidebar
- `?`: keyboard help
- `q`: quit in normal mode

## Architecture

- `src/app.rs`: application state, event loop, and persistence coordination
- `src/input.rs`: keyboard modes, navigation, and command actions
- `src/ui.rs`: Ratatui rendering only
- `src/provider.rs`: provider-neutral streaming and tool-event contract
- `src/opencode.rs`: documented OpenCode HTTP/SSE adapter
- `src/persistence.rs`: versioned SQLite migrations and atomic event recording
- `src/handoff.rs`: application-owned handoff context contracts
- `src/workspace.rs`: read-only Git/workspace integration and refresh data
- `src/terminal.rs`: terminal lifecycle and panic restoration

See [ARCHITECTURE.md](ARCHITECTURE.md) for runtime boundaries, persistence
semantics, provider contracts, and workspace safety rules.
