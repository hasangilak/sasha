# sasha

A terminal with Chrome-style vertical tabs, tab groups and VS Code-style workspaces, built with
[ratatui](https://ratatui.rs).

sasha runs inside your existing terminal app (Ghostty, iTerm2, Terminal.app …). Each
workspace is a folder with its own tabs, and each tab is a real PTY running your login shell.
Like tmux, the tabs live in a background server: close the window and they keep running, and
run `sasha` again to get them back.

```
▎ WD    ≡  web-dashboard          ⌕  │➜  web-dashboard git:(dev) yarn dev
        ───────────────────────────  │  VITE ready in 412 ms
  SA ⠹    ❯ dev server            ⚲  │  ➜  Local: http://localhost:5173/
          ✎ main.ts - NVIM        ⚲  │
  ~       ─────────────────────────  │
          ❯ web-dashboard         ×  │
  +       api                     ▾  │
        ▎ ⠹ cargo · infra         •  │
        ▎ ❯ api                      │
                     +               │
  ?    ^B tab keys · ^B ? help       │
```

## Run it

```sh
cargo run --release
# or install it on your PATH
cargo install --path .
sasha
```

## Your tabs keep running

`sasha` starts a background server the first time you run it. The server owns every shell; the
`sasha` in your window only shows them.

- **Close the window**, or press `^B d` to **detach**, and everything keeps running: dev servers,
  ssh sessions, vim, builds.
- **Run `sasha` again**, in any window, and it's all back as you left it, output included. Opening
  sasha in a second window moves it there, and the first window says so.
- Started from a project folder, sasha switches to that folder's workspace (adding it if it's
  new). Started from your home folder, where new terminal windows open, it carries on where you
  left off.
- `^B q` closes every tab and stops the server, and so does `sasha --kill-server`. The server
  also stops when its last tab closes.
- Logging out or restarting the Mac stops the server. Next time, your workspaces and pinned tabs
  come back as fresh shells, as before.
- After you rebuild sasha, the running server is still the old build, and the footer tells you.
  Quit with `^B q` and start sasha again to run the new one.

The server listens on `/tmp/sasha-<your uid>/default`, in a folder only you can open (set
`SASHA_SOCKET` to use another path). Its log sits next to it as `default.log`.

## Workspaces

The thin bar on the far left lists your workspaces as coloured initials (`WD` for
`web-dashboard-api`). Click one, or use `^B h` / `^B l`, to switch. Each workspace
keeps its own tabs, and new tabs open inside its folder.

- **Add** one with `+` in the bar or `^B w`, then type a path (`~/Work/…`). `Tab` completes
  folder names. `^B w` also offers the folder you're currently in.
- **Remove** one in `^B w` with `^D`. If it has open tabs, you press it twice.
- When you start sasha, it opens the workspace that contains the current folder, or adds that
  folder as a new workspace.
- A spinner or dot next to a badge means a tab in that workspace is printing, or printed
  something you haven't seen.

## Pinned tabs

Pin a tab with `^B p` or a right-click. Pinned tabs:

- sit at the top of their workspace, above a separator, marked with `⚲` (click it to unpin)
- need `x` pressed twice to close, and ignore middle-click
- **come back when sasha restarts**, in the folder they were last in and with their custom name

Dragging a tab across the separator pins or unpins it.

## Tab groups

Like Chrome's tab groups: a coloured header with a name, and a rail in the same colour beside
the tabs in the group.

- **Make one** with `^B m`: type a name and press Enter, and the tab moves into a new group. The
  same picker lists the groups you already have (`↓` and Enter adds the tab to one). Unnamed
  groups are fine too.
- `^B t` in a grouped tab opens the new tab **inside the group**. The `+` button always opens
  one at the bottom.
- **Collapse** a group by clicking its header or with `^B z`. A collapsed group shows how many
  tabs it hides and a dot or spinner when one of them prints something. `j` / `k` treat it as
  one stop that brings back the tab you last used there. That tab stays visible while it's
  active.
- **Rename** with a double-click on the header or `^B e`. While renaming, `Tab` changes the
  colour. A right-click on the header changes it too.
- **Move tabs in and out** by dragging them onto the group's tabs or header, or with `J` / `K`:
  at the group's edge the tab steps out, and next to a group it steps in (or jumps over a
  collapsed one). `^B u` takes the tab out of its group.
- `^D` in the `^B m` picker ungroups a whole group and leaves its tabs where they are. A group
  disappears when its last tab closes.

Pinned tabs can't be in a group (pinning a tab takes it out). Groups belong to their workspace
and, like the tabs in them, last until sasha quits.

## The tab sidebar

- **Titles** come from what's running: `sasha` (the folder) for an idle shell, `cargo · api`
  while a command runs, or the program's own title (vim, ssh). Rename a tab to fix its name.
- **Icons** stand in for favicons: `❯` shell, `✎` editor, `±` git, `⇄` ssh, `▸` build/runtime …
- A **spinner** means a background tab is printing output right now. A **blue dot** means it
  printed something you haven't seen, and a **yellow dot** means it rang the bell.
- Long titles fade out, like they do in Chrome.

## Colours

sasha uses your terminal's own background, so its colour, transparency, blur or background
image shows through the sidebars just like it does in the terminal pane. At startup sasha asks
the terminal for its foreground and background colours (OSC 10 / 11). The tab pills, buttons,
popups and text are mixed from those, and light themes get darker accent colours. If the
terminal doesn't answer, sasha falls back to Chrome's dark colours.

## Keys

Press the **prefix** (`Ctrl+B` by default) to enter **tab mode**. The active tab turns blue and
the keys below work until you press `Esc`.

| Tab mode | |
|---|---|
| `j` `k` / `↓` `↑` | next / previous tab |
| `h` `l` / `←` `→` | previous / next workspace |
| `1` … `9` | go to tab (`9` = last) |
| `Tab` / `Shift+Tab` | previous tab / previous workspace |
| `g` `G` | first / last tab |
| `J` `K` | move tab down / up |
| `t` `n` `c` | new tab (inside the tab's group) |
| `x` | close tab (pinned tabs: press twice) |
| `p` | pin / unpin tab |
| `m` | add tab to a group, or type a name for a new one (`^D` ungroups) |
| `u` | take tab out of its group |
| `z` | collapse / expand the tab's group |
| `e` | rename the tab's group (`Tab` changes its colour) |
| `r` `,` | rename tab (empty name = automatic title) |
| `w` | workspaces: switch, add a folder, `^D` to remove |
| `/` `f` | search tabs in every workspace (also by group name) |
| `[` `v` `PgUp` | scroll mode (`j` `k` `^u` `^d` `g` `G`, `q` exits) |
| `b` `s` | collapse / expand the tab sidebar |
| `a` | show / hide the workspace bar |
| `<` `>` | narrower / wider sidebar |
| prefix again | send the prefix to the shell |
| `?` | help |
| `d` | detach: the tabs keep running, `sasha` brings them back |
| `q` | quit: close every tab and stop the server |
| `Esc` `Enter` | back to the terminal |

| Anywhere | |
|---|---|
| `Ctrl+PgDn` / `Ctrl+PgUp` | next / previous tab |
| `Ctrl+Shift+PgDn` / `Ctrl+Shift+PgUp` | move tab down / up |
| `Alt+1` … `Alt+9` | go to tab |
| `Shift+PgUp` / `Shift+PgDn` | scroll back / forward |

| Mouse | |
|---|---|
| workspace badge / `+` | switch workspace / add a folder |
| click / drag a tab | switch / reorder (across the separator to pin or unpin) |
| right-click a tab | pin / unpin |
| click a group header | collapse / expand |
| double- / right-click a group header | rename / change colour |
| drag a tab onto a group | add it (drag it onto a plain tab or `+` to take it out) |
| `×` or middle-click | close |
| double-click a tab | rename |
| `⌕` / `≡` | search / collapse |
| drag the divider | resize the sidebar |
| wheel | scroll back through output |
| drag in the terminal | select and copy (double-click = word, triple-click = line) |
| `Shift`+drag | select even when the program uses the mouse (vim, htop) |

To use a different prefix, run `sasha --prefix ctrl-a` or set `SASHA_PREFIX=ctrl-a`.

On macOS, `Alt+…` shortcuts only work when your terminal sends Option as Alt
(Ghostty: `macos-option-as-alt = true`; iTerm2: Profiles → Keys → Left Option = Esc+).
The prefix keys work everywhere.

## Saved state

Workspaces and pinned tabs live in `~/.config/sasha/state.json` (or under `$XDG_CONFIG_HOME`).
Set `SASHA_STATE=/some/file.json` to use a different file. Restored pinned tabs start a fresh
shell. They don't re-run the command that was running before.

## How it works

| File | |
|---|---|
| `src/client.rs` | the `sasha` you run: starts the server, forwards input, prints what it draws |
| `src/server.rs` | the background server: owns the tabs, draws them for the attached client |
| `src/protocol.rs` | the messages between them, over a Unix socket |
| `src/tab.rs` | a shell in a PTY (`portable-pty`) mirrored by a `vt100` emulator; answers terminal queries |
| `src/workspace.rs` | a folder plus its tabs; badge initials and colour |
| `src/store.rs` | loading / saving workspaces and pinned tabs |
| `src/layout.rs` | screen layout and mouse hit-testing |
| `src/activity.rs` | the workspace bar |
| `src/sidebar.rs` | the tab strip |
| `src/ui.rs` | the terminal pane, pickers and popups |
| `src/app/` | event loop and state (`mod.rs`), keyboard (`keys.rs`), mouse (`mouse.rs`) |
| `src/input.rs` | turns key and mouse events back into xterm byte sequences |
| `src/procinfo.rs` | foreground process name and working directory (macOS, Linux) |
| `src/theme.rs` | the palette, derived from the terminal's colours; icons for programs |
| `src/termcolors.rs` | asks the terminal for its colours at startup |
| `src/clipboard.rs` | `pbcopy` / `wl-copy` / `xclip`, with OSC 52 as a fallback |

Shells see `TERM=xterm-256color` and `TERM_PROGRAM=sasha`.

## Limitations

- Text doesn't reflow when the window is resized (a `vt100` crate limitation).
- No inline images (kitty / sixel), ligatures or kitty keyboard protocol.
- One window at a time shows the tabs. Opening sasha in another window moves them there, where
  tmux could show them in both.
- Only workspaces and pinned tabs survive a logout or restart, as fresh shells.
