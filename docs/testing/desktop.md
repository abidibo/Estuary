# Desktop lifecycle: verification checklist (desktop M11.D13)

Use a throwaway config so your real setup is untouched:

```sh
export XDG_CONFIG_HOME=$(mktemp -d)        # config at $XDG_CONFIG_HOME/estuary/config.toml
export ESTUARY_LOG=estuary=debug
```

Useful commands:

- nodes: `pw-cli ls Node | grep 'node.name = "estuary\.'`
- autostart entry: `ls $XDG_CONFIG_HOME/autostart` (native builds) or `~/.config/autostart` (Flatpak portal)
- quit over D-Bus: `gdbus call --session --dest io.github.abidibo.Estuary --object-path /io/github/abidibo/Estuary --method org.gtk.Actions.Activate quit [] {}`
- signals: `kill -TERM $(pidof estuary)`; two quick `kill -INT`
- tray host present: `gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.kde.StatusNotifierWatcher`

## GNOME, native build (dev machine)

Verified by script on 2026-10-15 (Pop!_OS 24.04, GNOME Wayland, debug build):

| Check | Result |
|---|---|
| `estuary --background`: no window, channel nodes present | ✔ (no "window shown", 1 channel node) |
| Second `estuary` presents the window, exits 0, one process | ✔ exit 0 in 77 ms, `pgrep -xc estuary` = 1 |
| `start_at_login = true` writes `io.github.abidibo.Estuary.desktop` with the marker | ✔ |
| `start_at_login = false` removes our file | ✔ |
| A foreign autostart file is never modified or removed | ✔ (unit test `file_precedence_table`) |
| `kill -TERM`: exits, no `estuary.*` nodes, no dialog | ✔ 77 ms |
| Two quick `kill -INT` | ✔ exits, nothing left |
| `app.quit` over D-Bus, no usages | ✔ 55 ms, nothing left |
| `app.quit` with an app playing into a channel: window presented, confirmation waits; SIGTERM overrides it | ✔ exits 53 ms after SIGTERM, nothing left |
| Tray spawns where a StatusNotifierWatcher exists | ✔ (`tray present=true`) |
| First run shows the welcome dialog | ✔ (rendered) |

Needs a person at the window:

- [ ] ✕ with `close_action = "hide"` (default): the first ✕ shows "estuary keeps running", OK hides; `background_notice_shown = true` is saved; later ✕ hide silently; audio keeps flowing.
- [ ] ✕ with "Quit" chosen in Preferences runs the quit flow.
- [ ] Ctrl+Q / menu Quit with an app recording from the virtual mic: the dialog lists it; Cancel keeps everything; Quit removes all `estuary.*` nodes.
- [ ] Welcome → Recommended builds Music/Games/Chat + Virtual mic + noise suppression and opens the Matrix; Start empty (or ✕) opens Apps.
- [ ] Preferences (Ctrl+,): close action and Start at login; the switch writes/removes the autostart file.
- [ ] Tray: left click shows the window; "Show Estuary"; "Quit" runs the quit flow.
- [ ] Log out and in with start at login on: Estuary runs hidden and routes are active (needs `estuary` on `PATH`, open item O1).

## Flatpak (packaging M12)

- [ ] First start after the welcome shows GNOME's background permission dialog; allowed → listed in Background Apps with "Routing audio".
- [ ] Stopping it from Background Apps ends the process, no nodes left (records UNVERIFIED-D4: `app.quit` or kill).
- [ ] Denying background makes ✕ quit after the one-time explanation; Preferences shows the denial.
- [ ] Start at login through the portal; exactly one autostart entry (records UNVERIFIED-D2: file name and keys).
- [ ] Permission dialog without a parent window looks acceptable (UNVERIFIED-D5).

## COSMIC / KDE

- [ ] Tray in the status area (records UNVERIFIED-D6: name or pixmap); panel restart makes it disappear and return.
- [ ] Flatpak on COSMIC: Start at login insensitive with the "no background portal" explanation.
- [ ] KDE: Background portal path works; `set_status` failure on an older portal is silent (UNVERIFIED-D1).
