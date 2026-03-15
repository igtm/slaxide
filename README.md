# Slaxide

Linux-first Slack desktop client scaffold for a timeline-first, low-cognitive-load workflow.

## Workspace

- `crates/slaxide-core`: timeline models, ranking, themes, settings schema
- `crates/slaxide-store`: SQLite persistence and background actor
- `crates/slaxide-slack`: Slack OAuth, Socket Mode bootstrap, reply/upload client
- `crates/slaxide-platform`: notifications and secret-store abstractions
- `crates/slaxide-linux-gtk`: GTK demo shell for the Linux UI

## Verified locally

```bash
cargo test
```

This runs the default workspace members:

- `slaxide-core`
- `slaxide-store`
- `slaxide-slack`
- `slaxide-platform`

## GTK frontend

The Linux UI crate is present, but this environment does not have the GTK4 development packages needed by `pkg-config`.
The current shell is a themeable timeline demo driven by sample data; wiring it to the live Slack/store/platform layers is the next step once GTK can be built locally.

Current failing check:

```bash
cargo check -p slaxide-linux-gtk
```

Required system packages include the GTK4 and Graphene development files that provide `gtk4.pc` and `graphene-gobject-1.0.pc`.
