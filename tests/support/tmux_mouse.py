"""Exercise real SGR mouse input through an isolated tmux client and split pane.

Run after cargo build: python3 tests/support/tmux_mouse.py target/debug/timeliner-tui
Requires Unix, Python 3, and tmux. TMUX_BIN can select another tmux executable.
"""

import fcntl
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/timeliner-tui").resolve()
    tmux = os.environ.get("TMUX_BIN", "tmux")
    with tempfile.TemporaryDirectory(prefix="timeliner-mouse-") as directory:
        root = Path(directory)
        socket = root / "tmux.sock"
        config = root / "tmux.conf"
        config.write_text("set -g mouse on\nset -g status-position top\nset -g default-terminal tmux-256color\n")
        body = root / "source.body"
        body.write_text("".join(
            f"0|/mouse/event-{i:04}|{i}|mode|0|0|42|{1600000000+i}|{1600000000+i}|{1600000000+i}|{1600000000+i}\n"
            for i in range(500)
        ))
        rules = root / "rules.yaml"
        rules.write_text("[]\n")
        workspace = root / "workspace"
        subprocess.run(
            [str(binary), "--index-only", "--workspace", str(workspace),
             "--ioc-file", str(rules), str(body)],
            check=True, capture_output=True, timeout=20,
        )
        environment = os.environ.copy()
        environment.pop("TMUX", None)
        environment["TERM"] = "xterm-256color"
        client = None
        master = slave = None

        def command(*args):
            return subprocess.check_output(
                [tmux, "-S", str(socket), "-f", str(config), *args],
                env=environment, text=True, stderr=subprocess.STDOUT, timeout=5,
            ).rstrip("\n")

        def drain():
            while select.select([master], [], [], 0)[0]:
                try:
                    if not os.read(master, 65536):
                        break
                except OSError:
                    break

        def wait(predicate, description):
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                drain()
                result = predicate()
                if result:
                    return result
                time.sleep(0.03)
            raise AssertionError(f"Timed out waiting for {description}\n{capture()}")

        try:
            command("new-session", "-d", "-s", "mouse", "-x", "360", "-y", "42", "/bin/sh")
            invocation = shlex.join([str(binary), "--workspace", str(workspace)])
            pane = command(
                "split-window", "-h", "-p", "80", "-t", "mouse:0.0", "-P", "-F", "#{pane_id}",
                f"{invocation}; printf '\\nMOUSE_TEST_EXITED\\n'; exec /bin/sh",
            )
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 42, 360, 0, 0))

            def controlling_terminal():
                os.setsid()
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

            client = subprocess.Popen(
                [tmux, "-S", str(socket), "attach-session", "-t", "mouse"],
                stdin=slave, stdout=slave, stderr=slave, env=environment,
                preexec_fn=controlling_terminal,
            )
            os.close(slave)
            slave = None

            def capture():
                return command("capture-pane", "-p", "-t", pane)

            def flag(name):
                return command("display-message", "-p", "-t", pane, "#{" + name + "}")

            def send_mouse(button, x, y, release=False):
                # pane_top excludes the status line; the test deliberately puts
                # that line at the top and the application in the right pane.
                left = int(flag("pane_left"))
                top = int(flag("pane_top")) + 1
                sequence = f"\x1b[<{button};{left+x+1};{top+y+1}{'m' if release else 'M'}"
                os.write(master, sequence.encode("ascii"))

            def click(x, y, button=0):
                send_mouse(button, x, y)
                send_mouse(button, x, y, release=True)

            def selected_path():
                return next((line for line in capture().splitlines() if line.startswith("▌")), "")

            def click_label(label):
                # A menu/button click is a separate gesture, not another click
                # in the preceding row's double-click sequence.
                time.sleep(0.5)
                for y, line in enumerate(capture().splitlines()):
                    if label in line:
                        click(line.index(label) + 1, y)
                        return
                raise AssertionError(f"Missing clickable label: {label}\n{capture()}")

            wait(lambda: "/mouse/event-0000" in selected_path(), "the timeline")
            initial_width = int(flag("pane_width"))
            assert initial_width > 250, "The test must exercise SGR columns beyond 223"
            assert flag("mouse_any_flag") == "1"
            assert flag("mouse_button_flag") == "1"
            assert flag("mouse_all_flag") == "0", "Hover reporting must stay off"
            assert flag("mouse_sgr_flag") == "1"
            click(250, 10)
            wait(lambda: "/mouse/event-0005" in selected_path(), "row selection")
            send_mouse(65, 250, 10)
            wait(lambda: "/mouse/event-0003" in capture().splitlines()[5], "wheel scrolling down")
            assert "/mouse/event-0005" in selected_path()
            assert flag("pane_in_mode") == "0", "Wheel input must reach the app, not copy mode"
            send_mouse(64, 250, 10)
            wait(lambda: "/mouse/event-0000" in capture().splitlines()[5], "wheel scrolling up")
            assert "/mouse/event-0005" in selected_path()

            click(250, 10)
            time.sleep(0.06)
            click(250, 10)
            wait(lambda: "Record inspector" in capture(), "double-click inspection")
            assert "/mouse/event-0005" in capture()
            click_label("Esc close")
            wait(lambda: "Record inspector" not in capture(), "closing the inspector")

            click_label("2 IOC")
            wait(lambda: "Tab 2/2" in capture(), "switching tabs")
            click_label("1 Timeline")
            wait(lambda: "Tab 1/2" in capture() and "/mouse/event-0005" in selected_path(), "restoring the timeline")

            send_mouse(0, 250, 5)
            send_mouse(32, 250, 8)
            send_mouse(0, 250, 8, release=True)
            wait(lambda: "/mouse/event-0003" in selected_path(), "drag selection")
            # Separate gestures from tmux's multi-click recognition interval.
            time.sleep(0.5)
            click(250, 9, button=2)
            wait(lambda: "Record inspector" in capture(), "right-click inspection")
            assert "/mouse/event-0004" in capture()
            click_label("Esc close")
            wait(lambda: "Record inspector" not in capture(), "closing after right-click")

            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 300, 0, 0))
            os.kill(client.pid, signal.SIGWINCH)
            wait(lambda: int(flag("pane_width")) < initial_width, "tmux pane resize")
            click(int(flag("pane_width")) - 5, 8)
            wait(lambda: "/mouse/event-0003" in selected_path(), "selection after resize")
            if os.environ.get("TIMELINER_TMUX_CAPTURE"):
                Path(os.environ["TIMELINER_TMUX_CAPTURE"]).write_text(capture() + "\n")

            os.write(master, b"\x03")
            wait(lambda: "MOUSE_TEST_EXITED" in capture(), "normal exit")
            for name in ["mouse_any_flag", "mouse_button_flag", "mouse_all_flag", "mouse_sgr_flag", "alternate_on"]:
                assert flag(name) == "0", f"{name} was not restored"
            print(f"PASS: {command('-V')}; split pane, SGR >223, clicks, wheel, drag, tabs, resize, teardown")
        finally:
            if socket.exists():
                subprocess.run(
                    [tmux, "-S", str(socket), "kill-server"], env=environment,
                    capture_output=True, timeout=5,
                )
            if client is not None:
                try:
                    client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait(timeout=5)
            for descriptor in [master, slave]:
                if descriptor is not None:
                    os.close(descriptor)


if __name__ == "__main__":
    main()
