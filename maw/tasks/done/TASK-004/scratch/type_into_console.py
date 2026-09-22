"""TASK-004 spike (TEMPORARY): type text into another process's console.

SendKeys/AppActivate could not target the detached `claude` console (AppActivate
by pid returned false), so this attaches to that console directly and pushes key
events into its input buffer via WriteConsoleInputW.

Usage: python type_into_console.py <pid> <text>
       "\\r" in text = Enter, "\\e" = Escape
"""
import ctypes
import ctypes.wintypes as wt
import sys
import time

k32 = ctypes.WinDLL("kernel32", use_last_error=True)

GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
FILE_SHARE_READ = 1
FILE_SHARE_WRITE = 2
OPEN_EXISTING = 3
KEY_EVENT = 0x0001


class CHAR_UNION(ctypes.Union):
    _fields_ = [("UnicodeChar", wt.WCHAR), ("AsciiChar", ctypes.c_char)]


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [("bKeyDown", wt.BOOL), ("wRepeatCount", wt.WORD),
                ("wVirtualKeyCode", wt.WORD), ("wVirtualScanCode", wt.WORD),
                ("uChar", CHAR_UNION), ("dwControlKeyState", wt.DWORD)]


class INPUT_UNION(ctypes.Union):
    _fields_ = [("KeyEvent", KEY_EVENT_RECORD)]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [("EventType", wt.WORD), ("Event", INPUT_UNION)]


def records_for(text):
    recs = []
    vk_for = {"\r": 0x0D, "\x1b": 0x1B}
    for ch in text:
        for down in (1, 0):
            r = INPUT_RECORD()
            r.EventType = KEY_EVENT
            r.Event.KeyEvent.bKeyDown = down
            r.Event.KeyEvent.wRepeatCount = 1
            r.Event.KeyEvent.wVirtualKeyCode = vk_for.get(ch, 0)
            r.Event.KeyEvent.wVirtualScanCode = 0
            r.Event.KeyEvent.uChar.UnicodeChar = ch
            r.Event.KeyEvent.dwControlKeyState = 0
            recs.append(r)
    return recs


def main():
    pid = int(sys.argv[1])
    text = sys.argv[2].replace("\\r", "\r").replace("\\e", "\x1b")
    k32.FreeConsole()
    if not k32.AttachConsole(pid):
        sys.exit("AttachConsole failed: %d" % ctypes.get_last_error())
    h = k32.CreateFileW("CONIN$", GENERIC_READ | GENERIC_WRITE,
                        FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, 0, None)
    if h == wt.HANDLE(-1).value or h == -1:
        sys.exit("CreateFileW(CONIN$) failed: %d" % ctypes.get_last_error())
    recs = records_for(text)
    arr = (INPUT_RECORD * len(recs))(*recs)
    written = wt.DWORD(0)
    ok = k32.WriteConsoleInputW(wt.HANDLE(h), arr, len(recs), ctypes.byref(written))
    time.sleep(0.5)
    sys.exit(0 if ok else "WriteConsoleInputW failed: %d" % ctypes.get_last_error())


if __name__ == "__main__":
    main()
