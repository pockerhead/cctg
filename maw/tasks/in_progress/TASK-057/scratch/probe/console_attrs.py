"""TASK-057 probe: read another console's visible rows with their legacy
attribute words (ReadConsoleOutputW), like keys.rs visible_lines but with
attributes. Usage: python console_attrs.py <pid> <out_file>"""
import ctypes, ctypes.wintypes as wt, sys, json
k32 = ctypes.WinDLL("kernel32", use_last_error=True)
class COORD(ctypes.Structure): _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]
class SMALL_RECT(ctypes.Structure): _fields_ = [("Left", ctypes.c_short), ("Top", ctypes.c_short), ("Right", ctypes.c_short), ("Bottom", ctypes.c_short)]
class CSBI(ctypes.Structure): _fields_ = [("dwSize", COORD), ("dwCursorPosition", COORD), ("wAttributes", wt.WORD), ("srWindow", SMALL_RECT), ("dwMaximumWindowSize", COORD)]
class CHAR_INFO(ctypes.Structure): _fields_ = [("Char", wt.WCHAR), ("Attributes", wt.WORD)]
pid = int(sys.argv[1]); out = sys.argv[2]
k32.FreeConsole()
if not k32.AttachConsole(pid): sys.exit("attach failed")
h = k32.CreateFileW("CONOUT$", 0xC0000000, 3, None, 3, 0, None)
info = CSBI(); k32.GetConsoleScreenBufferInfo(wt.HANDLE(h), ctypes.byref(info))
w = info.dwSize.X; top, bottom = info.srWindow.Top, info.srWindow.Bottom
rows = []
for y in range(top, bottom + 1):
    buf = (CHAR_INFO * w)()
    rect = SMALL_RECT(0, y, w - 1, y)
    k32.ReadConsoleOutputW(wt.HANDLE(h), buf, COORD(w, 1), COORD(0, 0), ctypes.byref(rect))
    text = ''.join(c.Char for c in buf).rstrip()
    attrs = ['%04x' % c.Attributes for c in buf[:max(len(text), 1)]]
    rows.append({'y': y, 'text': text, 'attrs': attrs})
json.dump({'default_attr': '%04x' % info.wAttributes, 'cursor': [info.dwCursorPosition.X, info.dwCursorPosition.Y], 'rows': rows},
          open(out, 'w', encoding='utf-8'), ensure_ascii=False, indent=0)
