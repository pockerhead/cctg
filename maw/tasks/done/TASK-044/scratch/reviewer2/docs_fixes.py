# Reviewer-2 doc fixes (TASK-044): the stale "Windows only" sentences.
# Usage: python docs_fixes.py <workspace root>   (files are written with LF)
import sys

root = sys.argv[1]


def edit(path, pairs):
    full = f"{root}/{path}"
    s = open(full, encoding="utf-8").read().replace("\r\n", "\n")
    for old, new in pairs:
        assert s.count(old) == 1, (path, old[:60], s.count(old))
        s = s.replace(old, new)
    open(full, "w", encoding="utf-8", newline="\n").write(s)


edit("docs/poc.md", [
    (
        "работает только на Windows: агент пишет Esc во входной буфер консоли своего claude "
        "(`WriteConsoleInputW`; проверено в обычной консоли conhost, в Windows Terminal не проверено, "
        "в mintty без консоли не работает, тогда в тему приходит одно уведомление).",
        "работает на Windows: агент пишет Esc во входной буфер консоли своего claude "
        "(`WriteConsoleInputW`; проверено в обычной консоли conhost, в Windows Terminal не проверено, "
        "в mintty без консоли не работает, тогда в тему приходит одно уведомление). На Linux и macOS "
        "она работает, когда claude запущен через `claude-cctg` в терминале: Esc уходит в "
        "псевдотерминал `cctg run` (TASK-044).",
    ),
    (
        "(только Windows, пока не идёт ход и поле ввода пустое)",
        "(Windows, а на Linux и macOS claude, запущенный через `claude-cctg` в терминале; "
        "пока не идёт ход и поле ввода пустое)",
    ),
])

edit("README.md", [
    (
        "В Windows в обычной консоли (PowerShell, cmd, Windows Terminal) cctg нажимает за вас;",
        "В Windows в обычной консоли (PowerShell, cmd, Windows Terminal), на Linux и macOS "
        "в терминале cctg нажимает за вас;",
    ),
])
print("ok")
