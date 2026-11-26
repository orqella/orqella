# Dashboard theme

Orqella ships named themes and lets you layer a personal override on top. The
selected theme is stored in `config.toml` as:

```toml
theme = "nocturne"
```

Settings → General → Theme contains the picker. Switching themes swaps the
stylesheet in place: the current view, settings tab, and scroll position are
preserved. Newly opened module pages load the selected theme automatically.

## Built-in themes

| ID | Name | Character |
| --- | --- | --- |
| `nocturne` | Nocturne | Default. Deep indigo surfaces, soft text, and a blue-lavender accent. |
| `signal` | Signal | A high-contrast electric-blue night with violet highlights. |
| `daybreak` | Daybreak | Cool, light neutral surfaces for daylight work. |
| `foundry` | Foundry | Neutral graphite chrome with VS Code Dark Modern's default blue accent. |
| `cyber` | Cyber | A calm phosphor-green circuit night with pulsing traces, faint rain, and CRT scanlines. |
| `btop` | btop | The built-in btop++ terminal look: pitch black panes, monospace type, colored box borders with embedded titles, block meters, and a dark-red selection. |
| `night-city` | Night City | btop++'s palette and monospace type over Foundry's clean, geometric chrome. |

`btop` mirrors btop++'s built-in "Default" theme: a pure black terminal,
monospace type, square borders, btop's four box colors, dark-red selection
fills, and blocky green-to-red meters. `night-city` uses the same palette and
type but keeps the default geometric shell: rounded panes, plain borders, and
unsegmented meters.

The first-party Catppuccin themes were retired because their palettes
overlapped Nocturne and Daybreak. `catppuccin-mocha` and `catppuccin-latte`
remain accepted ids that resolve to Nocturne and Daybreak, so existing
`config.toml` selections keep working.

Theme files live in:

```text
web/themes/<id>.css
```

The default is `web/theme.css`, which is also what `/assets/theme.css` serves
when `nocturne` is selected. 

## Ambient motion

The native desktop client marks every page with
`html[data-orqella-shell="desktop"]`. On Linux, when WebKitGTK falls back to
its shared-memory transport, the client also sets
`html[data-orqella-motion="reduce"]`. That transport repaints the whole window
for every animation frame, so a continuous full-window animation can otherwise
consume a CPU core. Themes should honor that marker by keeping their static
appearance and stopping ambient animation. `cyber` does this for its circuit
backdrop. The shell also honors `prefers-reduced-motion`.

## User overrides

Any selected theme can be extended without forking it. Append CSS to:

```text
<config dir>/theme.css
```

Typical locations:

- Linux: `~/.config/orqella/theme.css`
- macOS: `~/Library/Application Support/orqella/theme.css`
- Windows: `%APPDATA%\orqella\theme.css`

The server appends that file after the selected theme, with
`Cache-Control: no-store`. It can override tokens and selectors, not just
colors. The Theme card in Settings shows the resolved path and has a **Reload**
button.

## Custom themes

Custom themes are complete standalone themes stored under:

```text
<config dir>/themes/<id>.css
<config dir>/themes/<id>.json
```

Custom theme IDs must start with `custom-`. The JSON sidecar contains the display metadata:

```json
{
  "name": "Moonlit",
  "description": "A deep blue night with warm amber highlights.",
  "color_scheme": "dark",
  "created_at": "2026-11-14T20:00:00.000Z"
}
```

Custom themes appear in the picker alongside the built-in themes. Selecting one
stores its id in `config.toml` as `theme = "custom-..."`. The personal override
still layers on top of the selected custom theme.

### Agent-generated themes

The Theme card has a prompt field for the configured default agent. The agent
runs in a staging workspace with the current theme as `base.css` and the token
contract as `THEME.md`. It writes:

- `theme.css` — a complete standalone theme with the full `:root` token set
- `theme.json` — the display name, description, and `color_scheme`

Orqella validates the CSS against the core token contract and rejects remote or
non-CSS additions (`@import`, `@font-face`, `url()`). It installs the result
under the custom themes directory, refreshes the picker, and selects the new
theme. It does not modify the personal override. Installed files are the
source of truth. There is no in-UI editor yet; to revise a generated theme,
edit its installed `<id>.css` and `<id>.json` and reload the dashboard, keeping
the id stable so the current selection keeps working. Generating again creates
another theme.

## Adding a built-in theme

1. Copy `web/theme.css` to `web/themes/<id>.css` and edit the variable block.
2. Register the theme in `src/theme.rs` in `THEMES` with an id, display name,
   description, and `color_scheme` (`dark` or `light`).
3. Keep the default id `nocturne` pointing at `web/theme.css`.
4. Run `cargo test --workspace` and select the new theme in Settings.
5. Check the shell, System, VM Lab, OpenCode, and Voice Assistant on desktop and
   narrow widths.

The token contract is intentionally broad enough for each theme to own the
entire surface without selector forks. Use the variables in `web/theme.css`;
do not hard-code palette values in `web/styles.css`.

Key tokens include:

```css
--bg --bg-soft --bg-inset --rail --rail-top
--panel --panel-2 --panel2 --panel-3
--line --line-soft --line-strong --border
--ink --fg --muted --muted-2
--mint --mint-dim --accent --accent-2 --green --amber --amber-2 --red --blue
--accent-soft --accent-faint --accent-border --accent-ink --accent-glow
--success-border --warn-border --danger-border
--input-bg --focus-border --focus-ring
--card-top --card-bottom --inset --hover
--overlay-bg --modal-bg --toast-bg --badge-ink
--onboarding-glow --ambient-a --ambient-b
--shadow --radius --radius-sm --rail-width --rail-collapsed-width
--font-sans --font-mono
```

## Themes and modules

A module that renders HTML/CSS should be congruent with the active theme by
default. Orqella exposes a small, token-driven component layer for modules at
`/assets/module.css`; it is the same visual vocabulary as the shell without the
shell's rail or page layout:

```html
<link rel="stylesheet" href="/assets/theme.css">
<link rel="stylesheet" href="/assets/module.css">
<link rel="stylesheet" href="style.css">
```

- Use the core primitives before writing new ones: `.card`, `.button`,
  `.field`, `.chip`, `.empty`, `.eyebrow`, `.lede`, `.row`, `.spread`, `.wrap`,
  `.muted`, `.small`, `.mono`, `.grid-2`, `.metric`, and `.icon-button`.
- Use theme tokens (`var(--panel)`, `var(--ink)`, `var(--mint)`, `var(--line)`,
  `var(--radius)`, `var(--font-sans)`, `var(--font-mono)`) instead of
  hard-coded palette values.
- The module's own stylesheet should cover layout and module-specific behavior,
  not re-create buttons, cards, forms, or typography.
- Prefer token overrides and `color-mix` over restyling shell selectors.
- Use `context.theme` (`dark` or `light`) and `context.theme_id` when the
  module needs to make a non-CSS styling decision.

This is a soft default. A user may explicitly request a different visual
direction, and a module may override a primitive when the core layer does not
provide an appropriate style. State the divergence and keep the module usable
in the selected theme.

If a module cannot consume the CSS system — for example a canvas, terminal,
chart, or native-feeling surface — derive its styling from the same tokens.
A terminal should use `--bg`, `--ink`, `--mint`, and `--font-mono` for its
background, foreground, cursor, and font rather than a fixed palette. Match the
current theme's density, corner radius, and accent placement without changing
the module's layout or hiding content.

Do not force theme congruence when the user explicitly asks for a different
visual direction. State the intentional divergence and keep the module usable
in the selected theme.
