# sasha

[![CI](https://github.com/hasangilak/sasha/actions/workflows/ci.yml/badge.svg)](https://github.com/hasangilak/sasha/actions/workflows/ci.yml)

A terminal with Chrome-style vertical tabs, tab groups and workspaces. Your tabs keep running
in the background, like tmux. Built with [ratatui](https://ratatui.rs), runs inside the
terminal you already use.

![sasha in Terminal.app: workspaces on the left, a tab group, and Claude Code running in a tab](docs/screenshot.png)

## Install

```sh
# Apple Silicon Mac (or sasha-macos-x86_64, sasha-linux-x86_64)
curl -fsSL https://github.com/hasangilak/sasha/releases/latest/download/sasha-macos-arm64.tar.gz | tar -xz
sudo mv sasha /usr/local/bin/
```

Or with Rust: `cargo install --git https://github.com/hasangilak/sasha`

## Use

Run `sasha` in a project folder. Press `Ctrl+B`, then a key (`Esc` goes back to typing):

| Key | |
|---|---|
| `t` | new tab |
| `j` `k` | next / previous tab |
| `h` `l` | previous / next workspace |
| `x` | close tab |
| `r` | rename tab |
| `p` | pin tab |
| `m` | add tab to a group |
| `z` | collapse / expand group |
| `w` | switch workspace, or add a folder |
| `/` | search tabs |
| `d` | detach: tabs keep running |
| `C` | keep the computer awake (☀), or let it sleep (☾) |
| `P` | open pull requests and their diffs (needs `gh`) |
| `O` | ports your tabs listen on; `x` `x` force-kills |
| `F` | browse the workspace's files: arrows, `⏎` reads one, `e` edits it |
| `i` | show / hide the ports and pull request bars |
| `q` | quit |
| `?` | every key |

The mouse works too: click and drag tabs, double-click to rename, right-click to pin, click a
link to open it, and drag any bar's edge to resize it.

## What it does

- **Workspaces** sort terminals by folder: a workspace lists every terminal in its folder
  (`cd` into another workspace's folder and the terminal moves there), and the sidebar stacks
  every workspace that has terminals open.
- **Groups** sort them by task: a named, coloured, collapsible set of terminals from any
  folders, listed at the top of the sidebar. A group stays, even empty, until you delete it
  (`×` on its header; `+` opens a terminal in it).
- **Sessions that last**: close the window or detach, then run `sasha` again and everything is
  still there. After a restart, your tabs come back as fresh shells.
- **Notifications** when a background tab finishes, rings the bell, or goes quiet (say, Claude
  is done).
- **Pull requests**: a bar right of the terminal lists the open PRs of the tab's repo (↻
  refreshes). Open one to see each file marked added, modified or deleted, with its diff. Needs the
  [GitHub CLI](https://cli.github.com), signed in.
- **Files**: a bar right of the terminal shows the workspace's folder as a tree, with changed
  files marked as git sees them. Click a file to read it over the terminal, or press `e` to open
  it in your `$EDITOR` in a new tab. `×` hides the bar; `Ctrl+B F` brings it back.
- **Ports**: a slim bar lists what the programs in your tabs listen on; click one to see which
  tab runs it, jump there, or force-kill it.
- **Keep awake**: click ☾ at the top of the workspace bar (☀ while it's on) and the computer
  won't sleep, hibernate or turn off and lock its screen, even with the window closed. It stays
  on across restarts until you switch it off.
- **Your terminal's colours**, transparency included.

Use another prefix with `sasha --prefix ctrl-a`. Turn notifications off with `SASHA_NOTIFY=0`.

## License

[MIT](LICENSE)
