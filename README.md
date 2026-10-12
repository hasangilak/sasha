# sasha

A terminal with Chrome-style vertical tabs, built with [ratatui](https://ratatui.rs).

sasha runs inside your existing terminal app (Ghostty, iTerm2, Terminal.app …) and gives
every shell its own tab in a sidebar. Each tab is a real PTY running your login shell.

```
  ≡                       ⌕  │➜  sasha git:(main) cargo build
 ─────────────────────────── │   Compiling sasha v0.1.0
  ❯ sasha                    │    Finished `dev` profile in 2.1s
  ⠹ cargo · api           •  │➜  sasha git:(main)
  ✎ main.rs - NVIM        ×  │
  ⇄ ssh · prod               │
              +              │
                             │
 ^B tab keys · ^B ? help     │
```

## Run it

```sh
cargo run --release
# or install it on your PATH
cargo install --path .
sasha
```

## The sidebar

- **Titles** come from what's running: `sasha` (the folder) for an idle shell, `cargo · api`
  while a command runs, or the program's own title (vim, ssh). Rename a tab to pin a name.
- **Icons** stand in for favicons: `❯` shell, `✎` editor, `±` git, `⇄` ssh, `▸` build/runtime …
- A **spinner** means a background tab is printing output right now. A **blue dot** means it
  printed something you haven't seen, and a **yellow dot** means it rang the bell.
- Long titles fade out, like they do in Chrome.

## Keys

Press the **prefix** (`Ctrl+B` by default) to enter **tab mode**. The active tab turns blue and
the keys below drive the tabs until you press `Esc`.

| Tab mode | |
|---|---|
| `j` `k` / `↓` `↑` | next / previous tab |
| `1` … `9` | go to tab (`9` = last) |
| `Tab` | back to the previous tab |
| `g` `G` | first / last tab |
| `J` `K` | move tab down / up |
| `t` `c` | new tab (opens in the current tab's folder) |
| `x` `w` | close tab |
| `r` `,` | rename tab (empty name = automatic title) |
| `/` `f` | search tabs |
| `[` `v` `PgUp` | scroll mode (`j` `k` `^u` `^d` `g` `G`, `q` exits) |
| `b` `s` | collapse / expand the sidebar |
| `h` `l` / `←` `→` | narrower / wider sidebar |
| prefix again | send the prefix to the shell |
| `?` | help |
| `q` | quit |
| `Esc` `Enter` | back to the terminal |

| Anywhere | |
|---|---|
| `Ctrl+PgDn` / `Ctrl+PgUp` | next / previous tab |
| `Ctrl+Shift+PgDn` / `Ctrl+Shift+PgUp` | move tab down / up |
| `Alt+1` … `Alt+9` | go to tab |
| `Shift+PgUp` / `Shift+PgDn` | scroll back / forward |

| Mouse | |
|---|---|
| click / drag a tab | switch / reorder |
| `×` or middle-click | close |
| double-click a tab | rename |
| `+` / `⌕` / `≡` | new tab / search / collapse |
| drag the divider | resize the sidebar |
| wheel | scroll back through output |
| drag in the terminal | select and copy (double-click = word, triple-click = line) |
| `Shift`+drag | select even when the program uses the mouse (vim, htop) |

To use a different prefix, run `sasha --prefix ctrl-a` or set `SASHA_PREFIX=ctrl-a`.

On macOS, `Alt+…` shortcuts only work when your terminal sends Option as Alt
(Ghostty: `macos-option-as-alt = true`; iTerm2: Profiles → Keys → Left Option = Esc+).
The prefix keys work everywhere.

## How it works

| File | |
|---|---|
| `src/tab.rs` | a shell in a PTY (`portable-pty`) mirrored by a `vt100` emulator; answers terminal queries |
| `src/sidebar.rs` | layout, hit-testing and drawing of the tab strip |
| `src/app.rs` | event loop, modes (normal / tab / scroll / rename / search) and input routing |
| `src/input.rs` | turns key and mouse events back into xterm byte sequences |
| `src/ui.rs` | draws the terminal pane and popups |
| `src/procinfo.rs` | foreground process name and working directory (macOS, Linux) |
| `src/clipboard.rs` | `pbcopy` / `wl-copy` / `xclip`, with OSC 52 as a fallback |

Shells see `TERM=xterm-256color` and `TERM_PROGRAM=sasha`.

## Limitations

- Text doesn't reflow when the window is resized (a `vt100` crate limitation).
- No inline images (kitty / sixel), ligatures or kitty keyboard protocol.
- Tabs live as long as sasha does; there's no detach / reattach like tmux.
