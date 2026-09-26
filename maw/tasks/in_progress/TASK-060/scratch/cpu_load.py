"""CPU load for flaky-test repro (TASK-060): N busy processes for S seconds.
usage: python cpu_load.py [procs=nproc+4] [seconds=600]"""
import multiprocessing as mp, os, sys, time

def burn(until):
    x = 0
    while time.time() < until:
        x = (x * 31 + 7) % 1000003

if __name__ == "__main__":
    n = int(sys.argv[1]) if len(sys.argv) > 1 else (os.cpu_count() or 4) + 4
    s = float(sys.argv[2]) if len(sys.argv) > 2 else 600
    until = time.time() + s
    ps = [mp.Process(target=burn, args=(until,)) for _ in range(n)]
    for p in ps: p.start()
    for p in ps: p.join()
