"""TASK-040 probe MCP server (TEMPORARY). Spawned by claude as a stdio MCP
server, i.e. exactly where `cctg agent` runs. Arg `channel` adds the
claude/channel capability. On the flag file <dir>/inject.flag with content
`exit` it types `/exit` and then (after 400 ms) Enter into its PARENT claude's
console exactly as keys.rs does for Esc: FreeConsole + AttachConsole(ppid) +
CreateFileW(CONIN$) + WriteConsoleInputW + FreeConsole. Logs to <dir>/mcp.log
(no session content)."""
import ctypes, ctypes.wintypes as wt, json, os, sys, threading, time

D = os.path.dirname(os.path.abspath(__file__))
LOG = os.path.join(D, 'mcp.log')
FLAG = os.path.join(D, 'inject.flag')
CHANNEL = 'channel' in sys.argv[1:]
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


def records(text):
    recs = []
    for ch in text:
        vk = {chr(13): 0x0D, chr(8): 0x08}.get(ch, 0)
        for down in (1, 0):
            r = INPUT_RECORD(); r.EventType = 1
            e = r.Event.KeyEvent
            e.bKeyDown = down; e.wRepeatCount = 1; e.wVirtualKeyCode = vk; e.wVirtualScanCode = 0
            e.uChar.UnicodeChar = ch; e.dwControlKeyState = 0
            recs.append(r)
    return recs


def parent_pid():
    class PE32(ctypes.Structure):
        _fields_ = [('dwSize', wt.DWORD), ('cntUsage', wt.DWORD), ('th32ProcessID', wt.DWORD),
                    ('th32DefaultHeapID', ctypes.POINTER(ctypes.c_ulong)), ('th32ModuleID', wt.DWORD),
                    ('cntThreads', wt.DWORD), ('th32ParentProcessID', wt.DWORD),
                    ('pcPriClassBase', ctypes.c_long), ('dwFlags', wt.DWORD), ('szExeFile', ctypes.c_char * 260)]
    snap = k32.CreateToolhelp32Snapshot(0x2, 0)
    pe = PE32(); pe.dwSize = ctypes.sizeof(PE32)
    names = {}
    if k32.Process32First(snap, ctypes.byref(pe)):
        while True:
            names[pe.th32ProcessID] = (pe.th32ParentProcessID, pe.szExeFile.decode(errors='replace'))
            if not k32.Process32Next(snap, ctypes.byref(pe)):
                break
    k32.CloseHandle(snap)
    pp = names.get(os.getpid(), (0, ''))[0]
    return pp, names.get(pp, (0, '?'))[1]


def write_console(ppid, text, tag):
    recs = records(text)
    k32.FreeConsole()
    if not k32.AttachConsole(ppid):
        log('%s AttachConsole(%d) failed err=%d' % (tag, ppid, ctypes.get_last_error()))
        return False
    try:
        h = k32.CreateFileW('CONIN$', 0xC0000000, 3, None, 3, 0, None)
        if h in (-1, 0xFFFFFFFFFFFFFFFF):
            log('%s CONIN$ failed err=%d' % (tag, ctypes.get_last_error())); return False
        arr = (INPUT_RECORD * len(recs))(*recs)
        n = wt.DWORD(0)
        ok = k32.WriteConsoleInputW(wt.HANDLE(h), arr, len(recs), ctypes.byref(n))
        k32.CloseHandle(wt.HANDLE(h))
        log('%s ok=%s written=%d' % (tag, bool(ok), n.value))
        return bool(ok)
    finally:
        k32.FreeConsole()


class COORD(ctypes.Structure):
    _fields_ = [('X', ctypes.c_short), ('Y', ctypes.c_short)]


class SMALL_RECT(ctypes.Structure):
    _fields_ = [('Left', ctypes.c_short), ('Top', ctypes.c_short), ('Right', ctypes.c_short), ('Bottom', ctypes.c_short)]


class CSBI(ctypes.Structure):
    _fields_ = [('dwSize', COORD), ('dwCursorPosition', COORD), ('wAttributes', wt.WORD),
                ('srWindow', SMALL_RECT), ('dwMaximumWindowSize', COORD)]


def input_box(ppid):
    """Lines strictly between the last two full-width rule lines of claude's
    visible window (the prompt input box), rstripped; None when not found."""
    k32.FreeConsole()
    if not k32.AttachConsole(ppid):
        return None
    try:
        h = k32.CreateFileW('CONOUT$', 0xC0000000, 3, None, 3, 0, None)
        info = CSBI()
        if not k32.GetConsoleScreenBufferInfo(wt.HANDLE(h), ctypes.byref(info)):
            return None
        width = info.dwSize.X
        lines = []
        for y in range(info.srWindow.Top, info.srWindow.Bottom + 1):
            buf = ctypes.create_unicode_buffer(width + 1)
            n = wt.DWORD(0)
            k32.ReadConsoleOutputCharacterW(wt.HANDLE(h), buf, width, COORD(0, y), ctypes.byref(n))
            lines.append(buf.value[:n.value].rstrip())
        k32.CloseHandle(wt.HANDLE(h))
    finally:
        k32.FreeConsole()
    rules = [i for i, l in enumerate(lines) if len(l) >= 20 and set(l) == {'─'}]
    if len(rules) < 2:
        return None
    a, b = rules[-2], rules[-1]
    return [l for l in lines[a + 1:b] if l.strip()]


def safe_exit(ppid):
    """Type /exit, look at the input box: exactly one line '❯ /exit' ->
    Enter; anything else -> five Backspaces (the typed text only) and no exit."""
    write_console(ppid, '/exit', 'safe: type /exit')
    time.sleep(0.4)
    box = input_box(ppid)
    log('safe: box=%r' % (box,))
    if box is not None and [l.replace(chr(0xa0), ' ').rstrip() for l in box] == ['❯ /exit']:
        write_console(ppid, chr(13), 'safe: Enter')
    else:
        write_console(ppid, chr(8) * 5, 'safe: 5x Backspace, no exit')
        time.sleep(0.4)
        log('safe: box after=%r' % (input_box(ppid),))


def watcher(ppid):
    while True:
        if os.path.exists(FLAG):
            try:
                kind = open(FLAG, encoding='utf-8').read().strip()
                os.remove(FLAG)
            except Exception:
                time.sleep(0.1); continue
            if kind == 'exit':
                write_console(ppid, '/exit', 'inject /exit')
                time.sleep(0.4)
                write_console(ppid, '\r', 'inject Enter')
            elif kind == 'safe_exit':
                safe_exit(ppid)
            elif kind == 'exit_one_write':
                write_console(ppid, '/exit\r', 'inject /exit+Enter in one write')
        time.sleep(0.1)


def main():
    ppid, pname = parent_pid()
    log('start parent=%d image=%s channel=%s sid=%s' % (ppid, pname, CHANNEL,
                                                         (os.environ.get('CLAUDE_CODE_SESSION_ID') or '')[:8]))
    threading.Thread(target=watcher, args=(ppid,), daemon=True).start()
    for line in sys.stdin:
        try:
            m = json.loads(line)
        except Exception:
            continue
        mid, meth = m.get('id'), m.get('method')
        if meth:
            log('in %s' % meth)
        if mid is None:
            continue
        if meth == 'initialize':
            pv = (m.get('params') or {}).get('protocolVersion') or '2025-06-18'
            caps = {'tools': {}}
            if CHANNEL:
                caps['experimental'] = {'claude/channel': {}}
            res = {'protocolVersion': pv, 'capabilities': caps, 'serverInfo': {'name': 'probe', 'version': '0'}}
            out = {'jsonrpc': '2.0', 'id': mid, 'result': res}
        elif meth == 'tools/list':
            out = {'jsonrpc': '2.0', 'id': mid, 'result': {'tools': []}}
        elif meth == 'ping':
            out = {'jsonrpc': '2.0', 'id': mid, 'result': {}}
        else:
            out = {'jsonrpc': '2.0', 'id': mid, 'error': {'code': -32601, 'message': 'no'}}
        sys.stdout.write(json.dumps(out) + '\n'); sys.stdout.flush()
    log('stdin EOF')


if __name__ == '__main__':
    main()
