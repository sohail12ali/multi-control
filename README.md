# Multi Control

Share one keyboard and mouse across Mac, Windows, and Linux computers on the same local network. There is no video and no remote desktop. Move the pointer off the edge of one screen and it continues on the computer you placed beside it.

Every computer runs the same agent. The machine you are sitting at captures the keyboard and mouse. The others only inject those events. A browser tab cannot do this: operating systems do not let a web page control the desktop.

## Arrange the desk

Open the panel and choose **Arrange**. Each computer is a rectangle sized from its resolution. Drag them left, right, above, or below each other, including a vertical offset when one screen sits higher. The pointer crosses only where two edges touch. A gap means it stays on the current machine.

You can also jump with **Ctrl+Alt** and an arrow, or by clicking a computer in the panel. The pointer waits briefly on a shared edge before it leaves, so a fast flick stays put. **Pause** returns input to the computer you are physically using. **Inputs** chooses which keyboard and mouse may leave this computer; the others stay local.

## Run it

Install [Rust](https://rustup.rs/) on each computer, then build the panel for that system. The panel starts the background daemon.

**macOS**

```bash
./scripts/bundle-macos.sh
open "target/Multi Control.app"
```

The menu-bar icon opens the panel. The first launch asks for Accessibility permission for **Multi Control**. Allow it in System Settings, then return to the panel. `cargo run -p multicontrol-macos` still builds the same programs for development, but macOS names the permission after the app bundle.

**Windows**

```bash
cargo run -p multicontrol-windows
```

No extra permission is required. A window that is running as Administrator ignores injected input unless Multi Control is elevated too.

**Linux**

Install GTK 4, then:

```bash
cargo run -p multicontrol-linux
```

On X11, capture and injection are available immediately. On GNOME and KDE Wayland, the desktop shows a portal permission dialog. Other compositors are not supported yet.

## Pair two computers

1. Open the panel on each machine. They find each other on the local network.
2. On one, choose **Add**. It shows a short code.
3. On the other, type that code.
4. Choose **Arrange** and drag the new computer to the side where it actually sits.
5. Move the pointer off that edge.

If discovery is blocked, the daemon also accepts a manual host and port. The default session port is `47320`.

## What is included

Discovery, code pairing, a permission check, the arrange canvas, edge crossing, hotkey jump, pause, and release of stuck keys when a computer disconnects.

Clipboard sync, multiple monitors on one computer, and keyboard-layout translation are not in this version.

## Layout

- `crates/core` holds the desk map, messages, and pairing records.
- `crates/platform` captures and injects input on each operating system.
- `crates/daemon` is the background process: discovery, encrypted sessions, and the local socket the panel uses.
- `crates/shell` is the shared client those panels use.
- `apps/macos`, `apps/windows`, and `apps/linux` are the native panels.

## License

MIT
