import json, sys
for line in open(sys.argv[1], encoding="utf-8"):
    line = line.strip()
    if line.endswith("NOAUTH"):
        print("NOAUTH!"); line = line[:-7]
    d = json.loads(line); e = d["event"]
    print(e["type"], {k: v for k, v in e.items() if k != "type"}, "host=", d["host"], "cwd=", "canonical-repo" if d["cwd"] == r"C:\Users\user\dev\cctg" else "OTHER")
