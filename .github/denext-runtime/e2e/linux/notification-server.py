#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
# A stand-in org.freedesktop.Notifications server for the Linux e2e run, on
# the run's private session bus (a hosted runner has no notification daemon).
# It accepts Notify / CloseNotification / GetCapabilities /
# GetServerInformation, emits NotificationClosed for a CloseNotification, and
# acts as the user for a notification whose body says so:
#
#   [[invoke:<key>]]  ActionInvoked(<id>, <key>) after 500 ms (a click on the
#                     body is the key "default", a button its action id)
#   [[dismiss]]       NotificationClosed(<id>, 2) after 500 ms
#
# Every Notify is appended to $E2E_NOTIFY_LOG (one JSON object per line), so
# the tests can see what reached the server. Prints "ready" once it owns the
# name. Mirrors laufey's backend-common/tests/mock_notification_server.cc.

import json
import os
import re
import sys

from gi.repository import Gio, GLib

XML = """
<node>
 <interface name='org.freedesktop.Notifications'>
  <method name='Notify'>
   <arg type='s' direction='in'/><arg type='u' direction='in'/>
   <arg type='s' direction='in'/><arg type='s' direction='in'/>
   <arg type='s' direction='in'/><arg type='as' direction='in'/>
   <arg type='a{sv}' direction='in'/><arg type='i' direction='in'/>
   <arg type='u' direction='out'/>
  </method>
  <method name='CloseNotification'><arg type='u' direction='in'/></method>
  <method name='GetCapabilities'><arg type='as' direction='out'/></method>
  <method name='GetServerInformation'>
   <arg type='s' direction='out'/><arg type='s' direction='out'/>
   <arg type='s' direction='out'/><arg type='s' direction='out'/>
  </method>
  <signal name='ActionInvoked'><arg type='u'/><arg type='s'/></signal>
  <signal name='NotificationClosed'><arg type='u'/><arg type='u'/></signal>
 </interface>
</node>
"""

PATH = "/org/freedesktop/Notifications"
IFACE = "org.freedesktop.Notifications"
LOG = os.environ.get("E2E_NOTIFY_LOG")
conn = None
next_id = [1]


def emit(signal, params):
    conn.emit_signal(None, PATH, IFACE, signal, params)


def later(ms, f):
    def run():
        f()
        return False

    GLib.timeout_add(ms, run)


def on_call(_conn, sender, _path, _iface, method, params, invocation):
    if method == "GetCapabilities":
        invocation.return_value(GLib.Variant("(as)", (["actions", "body"],)))
    elif method == "GetServerInformation":
        invocation.return_value(GLib.Variant("(ssss)", ("denext-e2e", "denext", "1", "1.2")))
    elif method == "CloseNotification":
        (nid,) = params.unpack()
        invocation.return_value(None)
        emit("NotificationClosed", GLib.Variant("(uu)", (nid, 3)))
    elif method == "Notify":
        app, replaces, _icon, summary, body, actions, hints, timeout = params.unpack()
        nid = replaces or next_id[0]
        if not replaces:
            next_id[0] += 1
        invocation.return_value(GLib.Variant("(u)", (nid,)))
        if LOG:
            with open(LOG, "a") as f:
                f.write(json.dumps({
                    "id": nid, "app": app, "replaces": replaces, "summary": summary,
                    "body": body, "actions": actions, "sender": sender,
                    "hints": sorted(hints.keys()), "timeout": timeout,
                    "desktopEntry": hints.get("desktop-entry"),
                }) + "\n")
        m = re.search(r"\[\[invoke:([^\]]+)\]\]", body)
        if m:
            key = m.group(1)
            later(500, lambda: emit("ActionInvoked", GLib.Variant("(us)", (nid, key))))
        if "[[dismiss]]" in body:
            later(500, lambda: emit("NotificationClosed", GLib.Variant("(uu)", (nid, 2))))
    else:
        invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)


def main():
    global conn
    conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    conn.register_object(PATH, node.interfaces[0], on_call, None, None)
    loop = GLib.MainLoop()

    def acquired(*_):
        print("ready", flush=True)

    def lost(*_):
        print("could not own org.freedesktop.Notifications", file=sys.stderr, flush=True)
        loop.quit()

    Gio.bus_own_name_on_connection(conn, IFACE, Gio.BusNameOwnerFlags.NONE, acquired, lost)
    loop.run()


main()
