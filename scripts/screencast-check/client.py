import dbus, dbus.mainloop.glib, subprocess, sys, json, time
from gi.repository import GLib
# safety first: no camera may exist in this PipeWire
nodes = json.loads(subprocess.run(["pw-dump"], capture_output=True, text=True).stdout or "[]")
cams = [n for n in nodes if n.get("type") == "PipeWire:Interface:Node" and "Video/Source" in str(n.get("info", {}).get("props", {}).get("media.class", "")) ]
print("camera/video sources before the request:", len(cams), flush=True)
if cams:
    print("ABORT: a video source exists, not reading anything", flush=True); sys.exit(2)
dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
sc = dbus.Interface(bus.get_object('org.freedesktop.portal.Desktop', '/org/freedesktop/portal/desktop'), 'org.freedesktop.portal.ScreenCast')
sender = bus.get_unique_name()[1:].replace('.', '_')
loop = GLib.MainLoop(); n = [0]
def req(fn, *args, opts):
    n[0] += 1; tok = f"t{n[0]}"; opts['handle_token'] = tok
    path = f"/org/freedesktop/portal/desktop/request/{sender}/{tok}"; res = {}
    def cb(code, results): res['r'] = (code, results); loop.quit()
    bus.add_signal_receiver(cb, 'Response', 'org.freedesktop.portal.Request', path=path)
    fn(*args, opts); GLib.timeout_add_seconds(15, loop.quit); loop.run()
    return res.get('r', (None, None))
code, r = req(sc.CreateSession, opts={'session_handle_token': 's1'}); print("CreateSession", code, flush=True)
session = r['session_handle']
code, r = req(sc.SelectSources, session, opts={'types': dbus.UInt32(1), 'multiple': False}); print("SelectSources", code, flush=True)
code, r = req(sc.Start, session, '', opts={}); print("Start", code, flush=True)
if code != 0: sys.exit(1)
node = int(r['streams'][0][0]); print("stream node", node, flush=True)
if node == 0:
    print("FAIL: the portal gave stream 0 (no stream)", flush=True); sys.exit(1)
time.sleep(0.5)
nodes = json.loads(subprocess.run(["pw-dump"], capture_output=True, text=True).stdout or "[]")
owner = [x for x in nodes if x.get("id") == node]
props = owner[0].get("info", {}).get("props", {}) if owner else {}
print("node", node, "class", props.get("media.class"), "name", props.get("node.name"), flush=True)
if not owner or "Video/Source" not in str(props.get("media.class")):
    print("FAIL: that node is not a video stream", flush=True); sys.exit(1)
fd = sc.OpenPipeWireRemote(session, {}).take()
# numbers only: brightness in the middle (white test window) and in a corner (background); no picture kept
import gi; gi.require_version('Gst', '1.0'); from gi.repository import Gst
Gst.init(None)
p = Gst.parse_launch(f"pipewiresrc fd={fd} path={node} num-buffers=5 ! videoconvert ! video/x-raw,format=GRAY8 ! appsink name=s sync=false")
s = p.get_by_name("s"); p.set_state(Gst.State.PLAYING); got = 0
for i in range(5):
    smp = s.emit("try-pull-sample", 5 * Gst.SECOND)
    if smp is None: break
    st = smp.get_caps().get_structure(0); w, h = st.get_value("width"), st.get_value("height")
    ok, m = smp.get_buffer().map(Gst.MapFlags.READ); b = m.data
    stride = len(b) // h
    mid = b[(h // 2) * stride + w // 2]
    corner = b[(h - 10) * stride + (w - 10)]
    smp.get_buffer().unmap(m); got += 1
    print(f"frame {got}: {w}x{h} test-window brightness {mid}, corner brightness {corner}", flush=True)
p.set_state(Gst.State.NULL)
print("RESULT:", "screen frames OK" if got and mid > 200 and corner < 100 else "not the expected picture", flush=True)
