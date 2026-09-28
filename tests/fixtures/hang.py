"""Prints once, then hangs silently forever (a deadlock stand-in)."""
import time

print("waiting for workers...")
while True:
    time.sleep(3600)
