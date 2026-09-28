"""Succeeds after a few seconds, with a \\r progress bar and some stderr."""
import sys
import time

print("loading data")
for i in range(0, 101, 5):
    sys.stdout.write(f"\rprogress {i:3d}% |{'#' * (i // 5):<20}|")
    sys.stdout.flush()
    time.sleep(0.1)
sys.stdout.write("\n")
print("warning: using default learning rate", file=sys.stderr)
print("done: loss=0.123")
