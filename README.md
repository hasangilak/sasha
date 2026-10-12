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
| `q` | quit |
| `?` | every key |

The mouse works too: click and drag tabs, double-click to rename, right-click to pin, click a
link to open it.

## What it does

- **Workspaces**: each folder has its own tabs. The sidebar stacks every workspace that has
  terminals open.
- **Tab groups**: named, coloured and collapsible, like in Chrome.
- **Sessions that last**: close the window or detach, then run `sasha` again and everything is
  still there. After a restart, your tabs come back as fresh shells.
- **Notifications** when a background tab finishes, rings the bell, or goes quiet (say, Claude
  is done).
- **Your terminal's colours**, transparency included.

Use another prefix with `sasha --prefix ctrl-a`. Turn notifications off with `SASHA_NOTIFY=0`.

## License

[MIT](LICENSE)
