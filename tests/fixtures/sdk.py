"""Reports structure through the Python helper and the TRUN_EVENTS side channel."""
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "sdk", "python"))
import trun  # noqa: E402

print("side channel enabled:", trun.enabled())
with trun.step("fit", "Fit model"):
    for i in range(1, 201):
        trun.metric(step=i, loss=1.0 / i, lr=1e-3)
        if i % 20 == 0:
            trun.progress("fit", i, 200, unit="iter")
        time.sleep(0.005)
trun.metric(step=201, loss=float("inf"))
trun.note("fit converged")
with trun.step("export", "Export"):
    trun.heartbeat()
print("bye")
