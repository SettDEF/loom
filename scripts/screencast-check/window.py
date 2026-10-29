import time
from Xlib import display, Xutil
d=display.Display(); s=d.screen()
w=s.root.create_window(0,0,600,400,0,s.root_depth,background_pixel=s.white_pixel)
w.set_wm_normal_hints(flags=Xutil.PMinSize|Xutil.PMaxSize,min_width=600,min_height=400,max_width=600,max_height=400)
w.map(); d.sync(); time.sleep(40)
