"""TASK-004 spike (TEMPORARY): dump another process's console screen buffer.

Used to capture what an interactive `claude` actually prints (startup banner,
`/mcp` screen), which is otherwise invisible when the session runs in its own
detached console.

Usage: python read_console_screen.py <pid> [out_file]
"""
import ctypes
import ctypes.wintypes as wt
import sys

k32 = ctypes.WinDLL("kernel32", use_last_error=True)

GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
FILE_SHARE_READ = 1
FILE_SHARE_WRITE = 2
OPEN_EXISTING = 3


class COORD(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


class SMALL_RECT(ctypes.Structure):
    _fields_ = [("Left", ctypes.c_short), ("Top", ctypes.c_short),
                ("Right", ctypes.c_short), ("Bottom", ctypes.c_short)]


class CONSOLE_SCREEN_BUFFER_INFO(ctypes.Structure):
    _fields_ = [("dwSize", COORD), ("dwCursorPosition", COORD), ("wAttributes", wt.WORD),
                ("srWindow", SMALL_RECT), ("dwMaximumWindowSize", COORD)]


def main():
    pid = int(sys.argv[1])
    out = sys.argv[2] if len(sys.argv) > 2 else None
    k32.FreeConsole()
    if not k32.AttachConsole(pid):
        sys.exit("AttachConsole failed: %d" % ctypes.get_last_error())
    h = k32.CreateFileW("CONOUT$", GENERIC_READ | GENERIC_WRITE,
                        FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, 0, None)
    if h == -1:
        sys.exit("CreateFileW(CONOUT$) failed: %d" % ctypes.get_last_error())
    info = CONSOLE_SCREEN_BUFFER_INFO()
    if not k32.GetConsoleScreenBufferInfo(wt.HANDLE(h), ctypes.byref(info)):
        sys.exit("GetConsoleScreenBufferInfo failed: %d" % ctypes.get_last_error())
    width, height = info.dwSize.X, info.dwSize.Y
    buf = ctypes.create_unicode_buffer(width)
    read = wt.DWORD(0)
    lines = []
    for y in range(height):
        if not k32.ReadConsoleOutputCharacterW(wt.HANDLE(h), buf, width,
                                               COORD(0, y), ctypes.byref(read)):
            break
        lines.append(buf[:read.value].rstrip())
    while lines and not lines[-1]:
        lines.pop()
    text = "\n".join(lines)
    if out:
        with open(out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(text + "\n")
    else:
        sys.stdout.buffer.write((text + "\n").encode("utf-8"))


if __name__ == "__main__":
    main()
