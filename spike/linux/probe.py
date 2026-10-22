# Minimal WebKitGTK host (same engine Tauri uses): enables WebRTC + media, loads the probe page.
import gi, sys
gi.require_version('Gtk', '3.0'); gi.require_version('WebKit2', '4.1')
from gi.repository import Gtk, WebKit2, GLib
win = Gtk.Window(); view = WebKit2.WebView(); s = view.get_settings()
for name in ('set_enable_webrtc', 'set_enable_media_stream', 'set_enable_mediasource'):
    if hasattr(s, name): getattr(s, name)(True)
print('WEBKIT', WebKit2.get_major_version(), WebKit2.get_minor_version(), WebKit2.get_micro_version(),
      'webrtc-setting', s.get_property('enable-webrtc') if s.find_property('enable-webrtc') else 'n/a', flush=True)
view.connect('permission-request', lambda v, r: (r.allow(), True)[1])
def on_title(v, _):
    t = v.get_title() or ''
    if t.startswith('DONE:'):
        print('PROBE_RESULT', t[5:], flush=True); Gtk.main_quit()
view.connect('notify::title', on_title)
win.add(view); view.load_uri('file:///probe/probe.html'); win.show_all()
GLib.timeout_add_seconds(80, Gtk.main_quit); Gtk.main()
