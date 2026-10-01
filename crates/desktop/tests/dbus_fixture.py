#!/usr/bin/env python3
"""Verify native D-Bus notification encoding and replacement on an isolated bus.

Ubuntu test dependencies: dbus-daemon, python3-dbus, python3-gi.
Run: python3 crates/desktop/tests/dbus_fixture.py [cargo-path]
No notification is sent to the user's desktop session.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading

if len(sys.argv) < 2 or sys.argv[1] != "--fixture":
    with tempfile.TemporaryDirectory(prefix="poknite-dbus-test-") as temporary:
        config = Path(temporary) / "bus.conf"
        config.write_text('''<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth><apparmor mode="disabled"/><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>''')
        cargo = sys.argv[1] if len(sys.argv) > 1 else "cargo"
        sys.exit(subprocess.call(["dbus-run-session", "--config-file=" + str(config), "--", sys.executable, __file__, "--fixture", cargo]))

import dbus
import dbus.service
import dbus.mainloop.glib
from gi.repository import GLib

dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
name = dbus.service.BusName("org.freedesktop.Notifications", bus)


class Fixture(dbus.service.Object):
    def __init__(self):
        super().__init__(name, "/org/freedesktop/Notifications")
        self.calls = 0

    @dbus.service.method("org.freedesktop.Notifications", in_signature="susssasa{sv}i", out_signature="u")
    def Notify(self, app, replaces, icon, title, body, actions, hints, expire):
        assert str(app) == "Poknite"
        assert str(title) == "Русский заголовок"
        assert str(body) == "&lt;текст&gt;&amp;"
        assert int(replaces) == (0 if self.calls == 0 else 42)
        assert list(actions) == [] and dict(hints) == {"suppress-sound": True}
        assert int(expire) == -1
        self.calls += 1
        return dbus.UInt32(42)


fixture = Fixture()
loop = GLib.MainLoop()
result = []


def run():
    result.append(subprocess.call([sys.argv[2], "test", "-p", "poknite-desktop", "native_method_and_replacement", "--", "--ignored"]))
    GLib.idle_add(loop.quit)


threading.Thread(target=run).start()
loop.run()
assert fixture.calls == 2
sys.exit(result[0])
