# wl-mitmproxy

`wl-mitmproxy` is a Wayland man-in-the-middle proxy built on top of `wl-proxy`.
It sits between a client and the compositor socket, forwards normal Wayland
traffic, and rewrites selected protocol messages in transit.

The current release focuses on `xdg_toplevel.set_app_id`. That makes it useful
when a Wayland client advertises an application ID that does not match the
desktop entry you want the shell to associate with the window. The project is
structured so additional message transformations can be added over time.

## What it does

`wl-mitmproxy` connects to the host Wayland display defined by `XDG_RUNTIME_DIR`
and `WAYLAND_DISPLAY`, then exposes a second Wayland socket for client
connections.

- In daemon mode, the proxy stays available until you stop it.
- In run mode, the proxy starts a child process, points that child at the proxy
	socket, and exits when the child exits.

When the rewritten app ID matches an installed `.desktop` entry, many desktop
shells can associate the window with the corresponding launcher metadata. In
practice, that often means the expected icon and taskbar grouping are used for
the proxied window.

## Build

```bash
cargo build --release
```

## Install

```bash
cargo install wl-mitmproxy
```

For a quick Wayland-native test client, [foot](https://codeberg.org/dnkl/foot)
is a good option because it is simple to launch directly through the proxy.

## Command syntax

Daemon mode:

```text
wl-mitmproxy --app-id <APP_ID> --proxy-socket <NAME_OR_PATH> [--proxy-runtime-dir <PATH>]
```

Run mode:

```text
wl-mitmproxy --app-id <APP_ID> \
	[--proxy-socket <NAME_OR_PATH>] \
	[--proxy-runtime-dir <PATH>] \
	-- <COMMAND> [ARGS...]
```

The `--` separator is the conventional boundary between `wl-mitmproxy` options
and the command that should be launched through the proxy.

## Daemon mode

Daemon mode is selected when no child command is provided after `--`.

In daemon mode, configure an explicit proxy socket name so client processes have
a stable `WAYLAND_DISPLAY` value to target.

```bash
wl-mitmproxy --app-id=firefox_firefox --proxy-socket=wayland-mitmproxy
```

The process detaches into the background and keeps the proxy socket available
until it is terminated.

To launch a client against that daemon socket:

```bash
WAYLAND_DISPLAY=wayland-mitmproxy foot --title Firefox
```

Socket selection in daemon mode:

- `--proxy-socket <name-or-path>` should be set explicitly in daemon mode.
- `WAYLAND_DISPLAY_PROXY` can be used as the source for that socket name when
  you prefer environment-based configuration.
- `--proxy-runtime-dir <path>` or `XDG_RUNTIME_DIR_PROXY` changes where relative
	proxy socket names are created.

Example:

```bash
wl-mitmproxy --app-id=firefox_firefox --proxy-socket=wayland-mitmproxy
```

```bash
WAYLAND_DISPLAY=wayland-mitmproxy foot --title Firefox
```

If you use a custom proxy runtime directory, clients must point at it
explicitly:

```bash
wl-mitmproxy \
	--app-id=firefox_firefox \
	--proxy-runtime-dir=/tmp/wl-mitmproxy \
	--proxy-socket=wayland-mitmproxy

XDG_RUNTIME_DIR=/tmp/wl-mitmproxy WAYLAND_DISPLAY=wayland-mitmproxy foot --title Firefox
```

## Run mode

Run mode is selected when a child command is provided after `--`.

```bash
wl-mitmproxy --app-id=firefox_firefox -- foot --title Firefox
```

In run mode the proxy:

- Creates a socket in `XDG_RUNTIME_DIR`, or in `XDG_RUNTIME_DIR_PROXY` /
	`--proxy-runtime-dir` when provided.
- Uses `--proxy-socket` or `WAYLAND_DISPLAY_PROXY` as an exact socket name when
	either is set.
- Otherwise allocates the first free name derived from `${WAYLAND_DISPLAY}-proxy`,
	for example `wayland-0-proxy`, `wayland-0-proxy-1`, and so on.
- Sets `WAYLAND_DISPLAY` for the spawned child to point to the proxy socket.
- Sets `XDG_RUNTIME_DIR` for the child only when a proxy runtime directory
	override is explicitly configured.
- Removes the proxy socket and exits once the child process terminates.

## Expected results

If the override is accepted by the client and the supplied app ID matches a
desktop entry installed on the system, the desktop shell may present the window
using the associated launcher icon and taskbar identity.

This is most reliable for applications that create their primary Wayland toplevel
window in the process started by `wl-mitmproxy`.

## Limitations

`wl-mitmproxy` can only rewrite Wayland messages sent through the proxied
connection. Some application models limit what that can achieve.

- Multi-process applications may create the visible window from a helper process
	or from an already running instance, bypassing the process you launched.
- Sandboxed packaging formats such as Snap may use launch wrappers, portals, or
	confinement rules that prevent the rewritten app ID from affecting the final
	window identity.
- If a client does not use `xdg_toplevel.set_app_id` in a way the proxy can
	intercept, there is no desktop integration change to apply.

## Exit behavior

- In daemon mode the process continues serving clients until it is terminated.
- In run mode the proxy exits with the child process status.
- Stale proxy sockets are reclaimed automatically when they are confirmed to be
	inactive.

## License

Everything in this repository is licensed under the GNU General Public License v3.0 except the wl-mitmproxy crate itself which is dual licensed under the MIT and Apache 2 licenses.