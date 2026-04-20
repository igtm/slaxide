---
name: slaxide
description: Guidance for working on Slaxide configuration, especially XDG config.toml overlays, search profiles, sections, channel permissions, notification rules, Slack credential precedence, and how these settings interact with the GTK Admin Config Editor and SQLite-backed settings.
---

# Slaxide

Use this skill when you need to explain, edit, generate, or debug Slaxide settings, especially:

- `~/.config/slaxide/config.toml`
- search/profile filtering
- sections, channel groups, and `times_*`-style channel matching
- notification rules and reusable profile composition
- channel read/write/hidden permissions
- Slack credential precedence between `.env`, environment variables, Settings UI, and TOML

## Primary files

Read only what you need:

- Example config: `docs/config.example.toml`
- True schema and matching logic: `crates/slaxide-core/src/settings.rs`
- Config loading and precedence: `crates/slaxide-linux-gtk/src/main.rs`

If the user asks for behavior and the example config is ambiguous, trust `settings.rs` and `main.rs`.

## Mental model

Slaxide settings come from 4 layers:

1. Built-in defaults in `AppSettings::default()`
2. SQLite-persisted settings
3. `XDG_CONFIG_HOME/slaxide/config.toml` overlay
4. For Slack credentials only: process env / `.env` at startup

Important consequences:

- `config.toml` is a startup overlay. It is loaded after SQLite and replaces matching fields in memory.
- UI edits made in Settings or Admin Config Editor persist to SQLite.
- If the same field also exists in `config.toml`, the TOML value wins again on next launch.
- Slack credentials have a separate rule:
  `exported env or .env > Settings-saved Slack values`

This means:

- Use `config.toml` for reproducible profile/filter/policy setup.
- Use Settings UI for iterative edits when you do not need them pinned in TOML.
- Use `.env` for local development secrets.

## Paths

Default XDG paths:

- Config: `~/.config/slaxide/config.toml`
- SQLite DB: `~/.local/share/slaxide/slaxide.db`
- Cache dir: `~/.cache/slaxide/`
- Avatar cache: `~/.cache/slaxide/avatars`
- Image cache: `~/.cache/slaxide/images`

If `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, or `XDG_CACHE_HOME` are set, Slaxide uses those instead.

## Config workflow

When asked to create or modify config:

1. Start from `docs/config.example.toml`
2. Keep IDs stable and ASCII
3. Prefer additive profile composition over large hard-coded channel lists
4. If the config is meant to override Admin UI edits, say so explicitly
5. If the change is local-dev-only, prefer `.env` for secrets and TOML for behavior
6. Validate TOML syntax after editing

Good validation command:

```bash
python3 -c 'import pathlib, tomllib; tomllib.loads(pathlib.Path("~/.config/slaxide/config.toml".replace("~", str(pathlib.Path.home()))).read_text()); print("ok")'
```

## Top-level keys

Common top-level keys in `config.toml`:

```toml
theme_id = "tokyo_night_storm"
active_search_profile_id = "release_focus"
default_channel_permission = "read_write"
```

Meaning:

- `theme_id`: current theme slug
- `active_search_profile_id`: profile automatically selected at startup
- `default_channel_permission`: fallback permission for channels not listed in `[channel_permissions]`

Allowed channel permissions:

- `read_write`
- `read_only`
- `hidden`

## Slack credentials

TOML supports:

```toml
[slack]
client_id = "..."
client_secret = "..."
redirect_uri = "https://127.0.0.1/slack/callback"
user_scopes = "channels:history,..."
app_token = "xapp-..."
```

But for development, prefer `.env` in the repo root:

```dotenv
SLAXIDE_SLACK_CLIENT_ID=...
SLAXIDE_SLACK_CLIENT_SECRET=...
SLAXIDE_SLACK_REDIRECT_URI=https://127.0.0.1/slack/callback
SLAXIDE_SLACK_USER_SCOPES=channels:history,channels:read,...
SLAXIDE_SLACK_APP_TOKEN=xapp-...
```

Precedence for these values:

1. exported environment variables
2. `.env` in the current working directory
3. Settings UI saved values

If the user says "Settings value is ignored", check whether `.env` or exported env is already set.

## Shortcuts

Shortcut overrides live under `[shortcuts]`:

```toml
[shortcuts]
open_settings = ["<Ctrl>comma"]
open_admin = ["<Ctrl><Shift>a"]
focus_search = ["<Ctrl>k"]
focus_composer = ["<Ctrl>n"]
close_column = ["<Ctrl>w", "Escape"]
```

These replace the saved bindings for the matching fields.

## Timeline policy

Timeline behavior is controlled by `[timeline]`:

```toml
[timeline]
watched_channels = ["C-eng", "C-release"]
muted_channels = ["C-random"]
focus_keywords = ["ship", "incident"]
focus_threshold = 75.0
recent_window_days = 7
```

And optional channel weights:

```toml
[timeline.channel_weights]
C-release = 3
C-incident = 4
```

Notes:

- If `watched_channels` is empty, `Recent` behaves like "all readable channels".
- `muted_channels` suppresses those channels in ranking/filtering paths.
- `focus_threshold` is a numeric ranking cutoff.
- `recent_window_days` controls the recent activity window.

## Channel permissions

Per-channel overrides:

```toml
[channel_permissions]
C-finance = "read_only"
C-secrets = "hidden"
```

Semantics:

- `hidden`: channel disappears entirely from the app
- `read_only`: still visible, but posting and replying are blocked
- `read_write`: normal behavior

Use this when the user wants guardrails without changing Slack permissions.

## Pattern matchers

Many config sections use `PatternMatcher`.

Two forms are supported:

```toml
{ text = "incident" }
{ regex = "^times_" }
```

Rules:

- `text` is a case-insensitive substring match
- `regex` uses Rust `regex`
- invalid regex is a settings error

When building matchers for non-technical users:

- prefer `text` when possible
- use `regex` only for prefixes, alternation, or anchored matching

## Profiles

Profiles are reusable building blocks.

### Keyword profiles

```toml
[[keyword_profiles]]
id = "shipping_terms"
label = "Shipping terms"
mode = "allow"
matchers = [
  { text = "ship" },
  { text = "release" },
]
```

`mode` is:

- `allow`
- `deny`

Use deny profiles for known noise like `wip`, drafts, or bots.

### Channel profiles

```toml
[[channel_profiles]]
id = "times_channels"
label = "Times channels"
mode = "allow"
channels = []
channel_name_matchers = [
  { regex = "^times_" },
]
```

Use cases:

- exact channel IDs in `channels`
- dynamic channel-name groups in `channel_name_matchers`

Important:

- `channel_name_matchers` operates on Slack channel names, not IDs
- it is the right tool for `times_*`, `proj_*`, `team-*` style groups
- public channels that match the active search profile can be pulled in even if the user is not a member
- private channels still require membership

### Section profiles

Sections are local channel groups. They are not Slack-native objects.

```toml
[[section_profiles]]
id = "release_section"
label = "Release section"
channels = ["C-release", "C-incident"]
channel_name_matchers = []
```

Use sections when the user wants a named bundle of channels that can be:

- reused in search profiles
- selected in the timeline top filter
- edited from Admin Config Editor

### Author profiles

```toml
[[author_profiles]]
id = "leadership"
label = "Leadership"
mode = "allow"
authors = ["U-lead", "U-pm"]
```

Use author profiles for people-centric filters and notification routing.

### Search profiles

Search profiles compose the other profiles:

```toml
[[search_profiles]]
id = "release_focus"
label = "Release Focus"
query = "ship"
keyword_profiles = ["shipping_terms", "wip_noise"]
section_profiles = ["release_section"]
channel_profiles = ["release_channels"]
author_profiles = ["leadership"]
```

Guidance:

- `query` is an additional free-text filter
- `section_profiles` and `channel_profiles` both constrain channels
- do not use inline channel/author allow-lists in new config
- set `active_search_profile_id` if you want this profile active on startup

For a "show only `times_*` channels" profile:

```toml
active_search_profile_id = "times_only"

[[channel_profiles]]
id = "times_channels"
label = "Times channels"
mode = "allow"
channels = []
channel_name_matchers = [
  { regex = "^times_" },
]

[[search_profiles]]
id = "times_only"
label = "Times only"
channel_profiles = ["times_channels"]
```

## Virtual office

The left rail has an `Office` view. It turns one channel profile into a live "desk" layout.

```toml
[office]
channel_profile_id = "times_channels"
```

Rules:

- each matched channel becomes one desk
- the latest cached post from that channel becomes the speech bubble
- Slack `conversation.creator` is used as the desk owner when available
- if `channel_profile_id` is unset, the app falls back to `times_channels` when that profile exists
- office channels are also prioritized during initial history sync so the view is populated early

Recommended setup:

- create a `channel_profiles` entry that matches `times_*`
- point `[office].channel_profile_id` at that profile
- optionally keep a separate `search_profile` for the same channels if you also want them in the main timeline

## Notification rules

Notification rules are independent from timeline display.

Example:

```toml
[[notification_rules]]
id = "cb4c4788-3cdb-4c74-b0aa-bd8db0c85c59"
label = "Release profile"
enabled = true
keyword_profile_ids = ["shipping_terms", "wip_noise"]
section_profile_ids = ["release_section"]
channel_profile_ids = ["release_channels"]
author_profile_ids = ["leadership"]
search_profile_ids = ["release_focus"]
thread_participation_only = false
action = "notify"
```

Optional quiet hours:

```toml
quiet_hours = { start_hour = 23, end_hour = 7 }
```

Actions:

- `notify`
- `silent`
- `critical`

Matching behavior:

- rule must be `enabled`
- referenced keyword/section/channel/author profiles must match
- if `search_profile_ids` is set, at least one referenced search profile must match

Use `thread_participation_only = true` for "only notify on threads I joined" rules.

## Admin Config Editor mapping

The GTK Admin Config Editor is a SQLite editor for TOML-like settings.

Map it like this:

- Config Editor `Sections` -> `[[section_profiles]]`
- `Channel Profiles` -> `[[channel_profiles]]`
- `Author Profiles` -> `[[author_profiles]]`
- `Search Profiles` -> `[[search_profiles]]`
- `Notification Rules` -> `[[notification_rules]]`
- `Channel Access` -> `[channel_permissions]` plus `default_channel_permission`

Important:

- Admin edits are not writing back to `config.toml`
- they persist in SQLite
- if TOML also defines the same setting, TOML wins again next launch

Current UI policy:

- `Search Profiles` should be composed from reusable profiles only
- `Notification Rules` should be composed from reusable profiles only
- do not introduce new inline channel, author, or matcher fields in those editors

So if the user wants source-controlled or reproducible behavior, edit TOML, not only Admin UI.

## Regex simulation

The Admin Config Editor can simulate regex/text matchers against cached messages.

Use it when:

- debugging a `PatternMatcher`
- testing a notification include/exclude rule
- estimating how broad a regex is before saving

Be explicit that simulation is against cached local messages, not the entire Slack workspace.

## Troubleshooting

### No messages after enabling a profile

Check:

- `active_search_profile_id` points to an existing profile
- the profile is not over-constrained by both channel and author filters
- `channel_permissions` did not hide the target channels
- the user is actually a member for private channels

### Private channels show as IDs or do not populate

Private channels still depend on membership and conversation metadata refresh. Do not assume public-channel behavior applies to them.

### Settings UI changes disappear on restart

Likely cause: the same field is defined in `config.toml`, so the overlay re-applies.

### Slack credential edits in Settings do nothing

Likely cause: `.env` or exported env is already providing the value and takes precedence.

### Regex is accepted in UI but behaves unexpectedly

Remember:

- Rust regex syntax
- matching is case-sensitive unless the regex says otherwise
- `text` matcher is usually safer for non-technical users

## Recommended authoring style

When asked to create config for a user:

- keep IDs short and stable
- use `label` for human-readable names
- prefer reusable profiles over one-off giant allow-lists
- use sections for user-facing grouping
- use `channel_name_matchers` for families like `times_*`
- use `channel_permissions` for local policy, not as a substitute for Slack ACLs

When asked to explain current behavior:

1. identify whether it comes from defaults, SQLite, TOML, or env
2. identify whether it affects display, notification, or permissions
3. call out if Admin UI edits are being shadowed by TOML
