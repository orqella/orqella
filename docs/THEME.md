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

Theme files live in:

```text
web/themes/<id>.css
```

The default is `web/theme.css`, which is also what `/assets/theme.css` serves
when `nocturne` is selected. 

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
