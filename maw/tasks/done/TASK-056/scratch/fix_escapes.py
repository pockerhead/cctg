"""Restores escapes in statusline.rs that a shell heredoc collapsed:
raw ESC bytes back to \\x1b, stray raw newlines inside string literals,
and single backslashes where Rust needs \\\\."""
p = r"C:/Users/user/dev/cctg-056/crates/cctg/src/statusline.rs"
s = open(p, encoding="utf-8").read()
s = s.replace("\x1b", "\\x1b")


def rep(a, b):
    global s
    assert s.count(a) == 1, a
    s = s.replace(a, b)


rep("        line.push('\n');", "        line.push('\\n');")
rep("cwd.trim_end_matches(['/', '\\']);", "cwd.trim_end_matches(['/', '\\\\']);")
rep(".rsplit(['/', '\\'])", ".rsplit(['/', '\\\\'])")
rep('("C:\\Users\\u\\dev\\cctg", "cctg")', '("C:\\\\Users\\\\u\\\\dev\\\\cctg", "cctg")')
rep("dir:app ctx:50%\n             acc:", "dir:app ctx:50%\\n\\\n             acc:")
rep("dir:app ctx:50%\n5h:3%", "dir:app ctx:50%\\n5h:3%")
rep("dir:w\n{shown}", "dir:w\\n{shown}")
# Inside a raw byte string the JSON must carry the \u001b escape itself.
rep('"a\\x1b[31m@b"', '"a\\u001b[31m@b"')
open(p, "w", encoding="utf-8", newline="\n").write(s)
