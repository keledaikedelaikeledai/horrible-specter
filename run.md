# Running Serangkai

## 1. Install Rust for the current terminal user

Run this in the terminal where `/root/.cargo` gave a permission error:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --profile minimal
```

Reload Rust in the current terminal:

```bash
source "$HOME/.cargo/env"
```

Check the installation:

```bash
cargo --version
rustc --version
```

## 2. Start Serangkai

```bash
cd /workspaces/codespaces-blank
cargo run
```

Launch it for another project:

```bash
cargo run -- /path/to/your/project
```

Show command help:

```bash
cargo run -- --help
```

## 3. TUI controls

- `i`: compose a message
- `Enter`: save the message locally
- `Shift+Enter`: add a line while composing
- `Esc`: leave compose mode
- `j` / `k`: navigate and scroll
- `h` / `l`: change focused panel
- `Tab` / `Shift+Tab`: cycle panels
- `Ctrl+p`: command palette
- `Ctrl+a`: provider selector
- `Ctrl+b`: toggle project sidebar
- `Ctrl+d`: toggle Git sidebar
- `?`: show keyboard help
- `q`: quit in normal mode

Messages are saved in `.serangkai/serangkai.db`.

The current milestone has no provider configured yet, so it saves messages
locally without pretending that an AI provider responded.
