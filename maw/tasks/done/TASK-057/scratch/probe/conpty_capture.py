"""TASK-057 probe: run the installed claude in a ConPTY (no window at all),
log every output byte, type into it through the pty input pipe. Same folder,
env and flags as drive_conhost.py. Usage: python conpty_capture.py <out.bin>"""
import ctypes, ctypes.wintypes as wt, json, os, shutil, subprocess, sys, threading, time
k32 = ctypes.WinDLL('kernel32', use_last_error=True)
HERE = os.path.dirname(os.path.abspath(__file__))
PROBE = os.path.join(os.environ['TEMP'], 'cctg-t016-probe')
MCP = os.path.join(HERE, 'empty_mcp.json')
CLAUDE = shutil.which('claude') or 'claude.exe'
class COORD(ctypes.Structure): _fields_ = [('X', ctypes.c_short), ('Y', ctypes.c_short)]
class STARTUPINFOW(ctypes.Structure):
    _fields_ = [('cb', wt.DWORD), ('lpReserved', wt.LPWSTR), ('lpDesktop', wt.LPWSTR), ('lpTitle', wt.LPWSTR),
                ('dwX', wt.DWORD), ('dwY', wt.DWORD), ('dwXSize', wt.DWORD), ('dwYSize', wt.DWORD),
                ('dwXCountChars', wt.DWORD), ('dwYCountChars', wt.DWORD), ('dwFillAttribute', wt.DWORD),
                ('dwFlags', wt.DWORD), ('wShowWindow', wt.WORD), ('cbReserved2', wt.WORD),
                ('lpReserved2', ctypes.c_void_p), ('hStdInput', wt.HANDLE), ('hStdOutput', wt.HANDLE), ('hStdError', wt.HANDLE)]
class STARTUPINFOEXW(ctypes.Structure): _fields_ = [('StartupInfo', STARTUPINFOW), ('lpAttributeList', ctypes.c_void_p)]
class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [('hProcess', wt.HANDLE), ('hThread', wt.HANDLE), ('dwProcessId', wt.DWORD), ('dwThreadId', wt.DWORD)]
k32.CreatePseudoConsole.argtypes = [COORD, wt.HANDLE, wt.HANDLE, wt.DWORD, ctypes.POINTER(ctypes.c_void_p)]
k32.InitializeProcThreadAttributeList.argtypes = [ctypes.c_void_p, wt.DWORD, wt.DWORD, ctypes.POINTER(ctypes.c_size_t)]
k32.UpdateProcThreadAttribute.argtypes = [ctypes.c_void_p, wt.DWORD, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_void_p]
k32.CreateProcessW.argtypes = [wt.LPCWSTR, wt.LPWSTR, ctypes.c_void_p, ctypes.c_void_p, wt.BOOL, wt.DWORD, ctypes.c_void_p, wt.LPCWSTR, ctypes.c_void_p, ctypes.c_void_p]
k32.ReadFile.argtypes = [wt.HANDLE, ctypes.c_void_p, wt.DWORD, ctypes.POINTER(wt.DWORD), ctypes.c_void_p]
k32.WriteFile.argtypes = [wt.HANDLE, ctypes.c_void_p, wt.DWORD, ctypes.POINTER(wt.DWORD), ctypes.c_void_p]
def pipe():
    r, w = wt.HANDLE(), wt.HANDLE()
    assert k32.CreatePipe(ctypes.byref(r), ctypes.byref(w), None, 0)
    return r, w
in_r, in_w = pipe(); out_r, out_w = pipe()
hpc = ctypes.c_void_p()
assert k32.CreatePseudoConsole(COORD(100, 30), in_r, out_w, 0, ctypes.byref(hpc)) == 0
size = ctypes.c_size_t()
k32.InitializeProcThreadAttributeList(None, 1, 0, ctypes.byref(size))
attrs = ctypes.create_string_buffer(size.value)
assert k32.InitializeProcThreadAttributeList(attrs, 1, 0, ctypes.byref(size))
assert k32.UpdateProcThreadAttribute(attrs, 0, 0x00020016, hpc, ctypes.sizeof(ctypes.c_void_p), None, None)
si = STARTUPINFOEXW(); si.StartupInfo.cb = ctypes.sizeof(STARTUPINFOEXW); si.StartupInfo.dwFlags = 0x100; si.lpAttributeList = ctypes.addressof(attrs)
pi = PROCESS_INFORMATION()
env = ''.join('%s=%s\0' % kv for kv in os.environ.items() if not kv[0].startswith('CLAUDE')) + '\0'
cmd = subprocess.list2cmdline([CLAUDE, '--setting-sources', 'project', '--strict-mcp-config', '--mcp-config', MCP])
ok = k32.CreateProcessW(None, ctypes.create_unicode_buffer(cmd), None, None, False, 0x00080000 | 0x00000400,
                        ctypes.create_unicode_buffer(env), PROBE, ctypes.byref(si), ctypes.byref(pi))
assert ok, ctypes.get_last_error()
out = open(sys.argv[1], 'wb'); log = []
def reader():
    buf = ctypes.create_string_buffer(65536); n = wt.DWORD()
    while k32.ReadFile(out_r, buf, 65536, ctypes.byref(n), None) and n.value:
        out.write(buf.raw[:n.value]); out.flush(); log.append(buf.raw[:n.value])
threading.Thread(target=reader, daemon=True).start()
def send(s):
    b = s.encode('utf-8'); n = wt.DWORD(); k32.WriteFile(in_w, b, len(b), ctypes.byref(n), None)
def mark(tag):
    out.write(b'\n\x00MARK ' + tag.encode() + b'\x00\n'); out.flush()
try:
    time.sleep(12); mark('idle')
    send('draft'); time.sleep(2); mark('draft'); send('\x7f' * 5); time.sleep(2); mark('erased')
    send('!'); time.sleep(0.3); send('echo ab'); time.sleep(2); mark('bash'); send('\x7f' * 8); time.sleep(1); send('\x1b'); time.sleep(1)
    send('/compact '); time.sleep(2); mark('slash'); send('\x7f' * 9); time.sleep(2); mark('end')
finally:
    subprocess.call(['taskkill', '/PID', str(pi.dwProcessId), '/T', '/F'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1); k32.ClosePseudoConsole(hpc); out.close()
