# Read-only timing probe for TASK-012 planning: process snapshot cost vs
# process spawn cost on this host. Writes nothing outside stdout.
import ctypes, ctypes.wintypes as wt, subprocess, time, statistics, os
TH32CS_SNAPPROCESS = 2
class PE(ctypes.Structure):
    _fields_ = [("dwSize", wt.DWORD), ("cntUsage", wt.DWORD), ("pid", wt.DWORD),
                ("heap", ctypes.POINTER(ctypes.c_ulong)), ("mod", wt.DWORD), ("thr", wt.DWORD),
                ("ppid", wt.DWORD), ("pri", ctypes.c_long), ("flags", wt.DWORD), ("exe", ctypes.c_char * 260)]
k32 = ctypes.windll.kernel32
k32.CreateToolhelp32Snapshot.restype = wt.HANDLE
def snap():
    h = k32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    e = PE(); e.dwSize = ctypes.sizeof(PE); n = 0
    ok = k32.Process32First(h, ctypes.byref(e))
    while ok:
        n += 1; ok = k32.Process32Next(h, ctypes.byref(e))
    k32.CloseHandle(h); return n
def bench(name, f, k=15):
    ts = []
    for _ in range(k):
        t = time.perf_counter(); r = f(); ts.append((time.perf_counter() - t) * 1000)
    print(f"{name}: median {statistics.median(ts):.1f} ms, max {max(ts):.1f} ms ({r})")
bench("toolhelp snapshot+iterate", snap)
exe = r"C:\Users\user\dev\cctg\target\release\cctg.exe"
bench("spawn cctg.exe hook SessionEnd (current no-op, tokio mt runtime)",
      lambda: subprocess.run([exe, "hook", "SessionEnd"], input=b"{}", capture_output=True).returncode)
bench("spawn powershell -NoProfile Get-CimInstance Win32_Process",
      lambda: subprocess.run(["powershell", "-NoProfile", "-Command",
            "Get-CimInstance Win32_Process -Filter \"ProcessId=$PID\" | Select -Expand ParentProcessId"],
            capture_output=True).returncode, k=3)
