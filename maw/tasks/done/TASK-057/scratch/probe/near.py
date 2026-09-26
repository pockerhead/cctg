"""TASK-057: print bundle bytes [a-w, a+w) as text."""
import sys, os
data = open(os.path.expanduser('~/.local/bin/claude.exe'), 'rb').read()
a = int(sys.argv[1]); w = int(sys.argv[2])
print(data[a-w:a+w].decode('utf-8','replace'))
