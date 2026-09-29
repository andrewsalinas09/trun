"""Reports a healthy loss, then NaN, then keeps running (health should go failing)."""
import time

for step in range(1, 4):
    print(f"::metric loss={1.0 / step:.3f} step={step}")
    time.sleep(0.5)
print("::metric loss=nan step=4")
print("loss exploded, still running")
for _ in range(30):
    print("::heartbeat")
    time.sleep(1)
