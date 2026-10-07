# maono

Control a **Maono PD100W** wireless microphone on Linux. Run `maono` for a live
terminal UI, or pass a subcommand to script it. No vendor app, no Wine, no audio-server tricks — it talks
to the receiver's raw HID node using the vendor protocol reverse-engineered from
Maono Link.

```
  battery         : 87%
  mute            : live
  gain            : 12 / 20
  noise reduction : on, mid
```

## What it can do

| Feature | TUI | CLI |
| --- | :-: | :-: |
| Battery percent | ✓ | ✓ |
| Mute / unmute | ✓ | ✓ |
| Gain, 0–20 | ✓ | ✓ |
| Noise reduction, off / low / mid / high | ✓ | ✓ |
| RGB light on/off and modes 0–8 | ✓ | ✓ |
| Live input level meter | ✓ | — |
| Waybar / status-bar JSON | — | ✓ |
| Omarchy bar widget | — | ✓ |
| Raw field read, write, and range scan | — | ✓ |

## Requirements

- Linux with `hidraw` (every distro kernel ships it).
- A udev rule so your user can open the device — see [Permissions](#permissions).
- Rust 1.85 or newer to build from source (the crate is edition 2024).

One binary, two dependencies (`ratatui` and `crossterm`), and it links nothing
but libc.

The receiver is found by its USB ids, not by port or device number, so any USB
port works and replugging is fine.

## Install

### Arch Linux / AUR

```sh
paru -S maono      # or: yay -S maono
```

Installs the prebuilt binary from the GitHub release, so there is nothing to
compile. The package drops the udev rule in place for you — replug the receiver
afterwards and you are done.

### From source

```sh
git clone https://github.com/shahriyardx/maono.git
cd maono
cargo build --release

sudo install -Dm755 target/release/maono /usr/local/bin/maono
sudo install -Dm644 99-maono.rules /etc/udev/rules.d/99-maono.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Unplug and replug the receiver, then run `maono`.

## Permissions

Without the udev rule you get:

```
maono: /dev/hidraw3 needs the udev rule - see 99-maono.rules
```

`99-maono.rules` matches USB `352f:0414` (the wireless receiver) and
`352f:0417` (the mic plugged in by cable) and does two things: it tags the node
with `uaccess`, which hands it to the user logged in at the local seat, and it
sets group `input` as a fallback for headless logins. If you rely on the group
fallback, add yourself to it once and log out and back in:

```sh
sudo usermod -aG input "$USER"
```

`sudo maono` also works, but you do not need it.

## Usage

```
maono                     live terminal UI (default)
maono status [--json]     battery, mute, gain, noise reduction
maono mute | unmute | toggle
maono gain [n | +n | -n]  0-20
maono nr [off | low | mid | high]
maono light [on | off | next | 0-8 | <colour>]

maono get <id>            read one raw field, e.g. 0x208e
maono set <id> <value>    write one raw field
maono scan [lo] [hi]      dump a field range (read-only)
```

Running `maono` with no arguments opens the TUI, which is the everyday way to
use it. The subcommands are there for scripting and for status bars.

Examples:

```sh
maono                 # full-screen live view
maono toggle          # mute or unmute
maono gain +2         # nudge gain up
maono nr high
maono light next      # cycle the RGB mode
maono light purple    # or by colour name
```

### TUI keys

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab` | Move between controls |
| `←` `→` (or `-` `+`) | Change the focused value |
| `Enter` / `Space` | Toggle the focused control |
| `m` | Mute / unmute |
| `l` | RGB light on / off |
| `Home` / `End` | Gain to 0 / 20 |
| `0`–`3` | Noise reduction off, low, mid, high |
| `q` / `Esc` | Quit |

The mouse works too: click or drag the sliders and switches.

### Releasing

Publishing a GitHub release does the rest. `.github/workflows/release.yml`
builds the binary in an Arch container, attaches it to that release, renders
`packaging/PKGBUILD.in` with the new version and checksums, and pushes the
result to the AUR.

```sh
# bump version in Cargo.toml first, then:
git tag -a v0.3.0 -m "maono 0.3.0" && git push origin v0.3.0
gh release create v0.3.0 --generate-notes
```

The workflow refuses to run if the tag and `Cargo.toml` disagree. It needs one
repository secret, `AUR_SSH_PRIVATE_KEY`, holding a key registered with the AUR
account. `workflow_dispatch` re-runs it against an existing tag.

## Omarchy bar widget

A Quickshell bar widget for the [Omarchy](https://omarchy.org) shell: mic icon
in the bar, and a panel with battery, a mute switch, a gain slider, noise
reduction and the RGB light.

```sh
maono shell install
omarchy-shell shell rescanPlugins
omarchy plugin enable maono
```

`maono shell install` copies the widget into
`~/.config/omarchy/plugins/maono/`, because the shell only discovers plugins
there and a package cannot write to your config at build time. Pass `--force`
to overwrite an existing copy, and `maono shell uninstall` to remove it.

Mute sits in the panel header. Below it: a gain slider, noise reduction as four
chips, the light switch, and the nine light colours as chips right under it.

Keys inside the panel: `j` / `k` move between controls, `h` / `l` change the
selected one, `m` mutes, `b` toggles the light, `n` steps to the next colour,
`r` reloads. `h j k l` belong to the shell's own navigation, which is why the
light is on `b`.

## Status bar

`maono status --json` prints one line for Waybar and friends:

```json
{"text":"󰍬","class":"live","tooltip":"Mic live · battery 87% · gain 12/20 · NR on, mid","muted":false,"battery":87,"gain":12,"gain_max":20,"nr":"on, mid","light_on":true,"light_mode":3}
```

`text` is a Nerd Font glyph and `class` is `live` or `muted`, so you can style
it in CSS. A Waybar module looks like this:

```jsonc
"custom/mic": {
  "exec": "maono status --json",
  "return-type": "json",
  "interval": 5,
  "on-click": "maono toggle"
}
```

## Raw fields

Field ids are slot-based: `0x2000` is transmitter 1, `0x2800` transmitter 2,
`0x3000` the receiver. Known ids:

| Id | Meaning |
| --- | --- |
| `0x0042` | Battery percent |
| `0x0044` | Input level, streamed ~10×/sec |
| `0x207d` | Mute, 1 = muted |
| `0x207e` | Gain, 0–20 |
| `0x2084` | Noise reduction on/off |
| `0x2085` | Noise reduction level, 0–2 |
| `0x2089` | RGB light on/off |
| `0x208c` | RGB light mode, 0–8 |

Light modes are colours, in the order the light button cycles them: `0` white,
`1` red, `2` orange, `3` lime, `4` green, `5` cyan, `6` blue, `7` purple,
`8` light blue. `maono light <colour>` takes any of those names.

**Careful:** the firmware validates nothing it is sent. `set` stores any 16-bit
value at any id, including ids nobody has mapped yet. `scan` is read-only and
safe; `set` on an unknown id is not. Poke at your own risk.

## How it works

Every frame is 64 bytes on the receiver's hidraw node:

```text
c4 <len> 00 00 <type> [id_lo id_hi val_lo val_hi] <ck_lo ck_hi>   padded to 64
```

`len` counts the whole frame including the leading `c4` and the checksum, and
`ck` is the two's complement of everything before it. Type `0x03` writes, `0x04`
reads, and the device uses `0x03` to announce changes — which is how the TUI
notices you pressing the buttons on the mic itself.

Protocol details live in `src/mic.rs`, and every frontend drives the device
through that one module.

## Releasing

Publishing a GitHub release does the rest. `.github/workflows/release.yml`
builds the binary in an Arch container, attaches it to that release, renders
`packaging/PKGBUILD.in` with the new version and checksums, and pushes the
result to the AUR.

```sh
# bump version in Cargo.toml first, then:
git tag -a v0.3.0 -m "maono 0.3.0" && git push origin v0.3.0
gh release create v0.3.0 --generate-notes
```

The workflow refuses to run if the tag and `Cargo.toml` disagree. It needs one
repository secret, `AUR_SSH_PRIVATE_KEY`, holding a key registered with the AUR
account. `workflow_dispatch` re-runs it against an existing tag.

## Status

Tested against one PD100W receiver on Arch Linux, and against a PD100W plugged
in by cable (USB `352f:0417`), which speaks the same protocol on the same field
slot. Other Maono receivers use different USB ids and are not detected; if you
have one, the id to add is in `HID_MATCH` in `src/mic.rs`.

Unofficial and unaffiliated with Maono.

## License

MIT
