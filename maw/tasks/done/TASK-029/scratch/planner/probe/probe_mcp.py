"""TASK-029 probe MCP server (TEMPORARY). Spawned by claude as a stdio MCP
server, i.e. exactly where `cctg agent` runs. Records its own console situation
at start, then on a flag file `<dir>/inject.flag` (content: esc | ctrlb)
injects that key into its PARENT's console: FreeConsole + AttachConsole(ppid)
+ CreateFileW(CONIN$) + WriteConsoleInputW, then FreeConsole again.
Logs to <dir>/mcp.log (no content of the session)."""
import ctypes, ctypes.wintypes as wt, json, os, sys, threading, time

D = os.path.dirname(os.path.abspath(__file__))
LOG = os.path.join(D, 'mcp.log')
FLAG = os.path.join(D, 'inject.flag')
k32 = ctypes.WinDLL('kernel32', use_last_error=True)


def log(msg):
    with open(LOG, 'a', encoding='utf-8') as f:
        f.write('%.3f %d %s\n' % (time.time(), os.getpid(), msg))


class CHAR_UNION(ctypes.Union):
    _fields_ = [('UnicodeChar', wt.WCHAR), ('AsciiChar', ctypes.c_char)]


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [('bKeyDown', wt.BOOL), ('wRepeatCount', wt.WORD), ('wVirtualKeyCode', wt.WORD),
                ('wVirtualScanCode', wt.WORD), ('uChar', CHAR_UNION), ('dwControlKeyState', wt.DWORD)]


class INPUT_UNION(ctypes.Union):
    _fields_ = [('KeyEvent', KEY_EVENT_RECORD)]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [('EventType', wt.WORD), ('Event', INPUT_UNION)]


KEYS = {  # vk, scan, char, ctrl state
    'esc': (0x1B, 0x01, '\x1b', 0),
    'ctrlb': (0x42, 0x30, '\x02', 0x0008),  # LEFT_CTRL_PRESSED
}


def parent_pid():
    class PE32(ctypes.Structure):
        _fields_ = [('dwSize', wt.DWORD), ('cntUsage', wt.DWORD), ('th32ProcessID', wt.DWORD),
                    ('th32DefaultHeapID', ctypes.POINTER(ctypes.c_ulong)), ('th32ModuleID', wt.DWORD),
                    ('cntThreads', wt.DWORD), ('th32ParentProcessID', wt.DWORD),
                    ('pcPriClassBase', ctypes.c_long), ('dwFlags', wt.DWORD), ('szExeFile', ctypes.c_char * 260)]
    snap = k32.CreateToolhelp32Snapshot(0x2, 0)
    pe = PE32(); pe.dwSize = ctypes.sizeof(PE32)
    me, res = os.getpid(), None
    names = {}
    if k32.Process32First(snap, ctypes.byref(pe)):
        while True:
            names[pe.th32ProcessID] = (pe.th32ParentProcessID, pe.szExeFile.decode(errors='replace'))
            if not k32.Process32Next(snap, ctypes.byref(pe)):
                break
    k32.CloseHandle(snap)
    pp = names.get(me, (0, ''))[0]
    return pp, names.get(pp, (0, '?'))[1]


def console_info(tag):
    hwnd = k32.GetConsoleWindow()
    arr = (wt.DWORD * 64)()
    n = k32.GetConsoleProcessList(arr, 64)
    log('%s console_hwnd=%s console_procs=%d' % (tag, hex(hwnd or 0), n))


def inject(kind, ppid):
    vk, scan, ch, st = KEYS[kind]
    recs = []
    for down in (1, 0):
        r = INPUT_RECORD(); r.EventType = 1
        e = r.Event.KeyEvent
        e.bKeyDown = down; e.wRepeatCount = 1; e.wVirtualKeyCode = vk; e.wVirtualScanCode = scan
        e.uChar.UnicodeChar = ch; e.dwControlKeyState = st
        recs.append(r)
    k32.FreeConsole()
    if not k32.AttachConsole(ppid):
        log('inject %s AttachConsole(%d) failed err=%d' % (kind, ppid, ctypes.get_last_error()))
        return
    try:
        h = k32.CreateFileW('CONIN$', 0xC0000000, 3, None, 3, 0, None)
        if h in (-1, 0xFFFFFFFFFFFFFFFF):
            log('inject %s CONIN$ failed err=%d' % (kind, ctypes.get_last_error())); return
        arr = (INPUT_RECORD * len(recs))(*recs)
        n = wt.DWORD(0)
        ok = k32.WriteConsoleInputW(wt.HANDLE(h), arr, len(recs), ctypes.byref(n))
        k32.CloseHandle(wt.HANDLE(h))
        log('inject %s ok=%s written=%d' % (kind, bool(ok), n.value))
    finally:
        k32.FreeConsole()


def watcher(ppid):
    while True:
        if os.path.exists(FLAG):
            try:
                kind = open(FLAG, encoding='utf-8').read().strip()
                os.remove(FLAG)
            except Exception:
                time.sleep(0.1); continue
            if kind in KEYS:
                inject(kind, ppid)
        time.sleep(0.1)


def main():
    ppid, pname = parent_pid()
    log('start parent=%d image=%s CLAUDE_PID=%s' % (ppid, pname, os.environ.get('CLAUDE_PID')))
    console_info('start')
    threading.Thread(target=watcher, args=(ppid,), daemon=True).start()
    for line in sys.stdin:
        try:
            m = json.loads(line)
        except Exception:
            continue
        mid, meth = m.get('id'), m.get('method')
        if mid is None:
            continue
        if meth == 'initialize':
            pv = (m.get('params') or {}).get('protocolVersion') or '2025-06-18'
            res = {'protocolVersion': pv, 'capabilities': {'tools': {}},
                   'serverInfo': {'name': 'probe', 'version': '0'}}
            out = {'jsonrpc': '2.0', 'id': mid, 'result': res}
        elif meth == 'tools/list':
            out = {'jsonrpc': '2.0', 'id': mid, 'result': {'tools': []}}
        elif meth == 'ping':
            out = {'jsonrpc': '2.0', 'id': mid, 'result': {}}
        else:
            out = {'jsonrpc': '2.0', 'id': mid, 'error': {'code': -32601, 'message': 'no'}}
        sys.stdout.write(json.dumps(out) + '\n'); sys.stdout.flush()


if __name__ == '__main__':
    main()
